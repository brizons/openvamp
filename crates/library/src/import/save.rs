//! Saving a draft: the song folder built in a hidden folder of the library,
//! checked with the player's reader, and renamed into place.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

use domain::{PackagedDraft, Stem};

use super::audio::{self, AudioFacts, TranscodeError};
use super::source::{self, Scratch};
use super::{ImportDraft, ImportError, ImportProgress, Location, ProbedStem, Source, StemAction};
use crate::songs::{SongEntry, entry};
use crate::{
    Library, LoadError, MANIFEST_FILE, open_package, open_regular_file, open_stem, problem_stem,
    read_through, resolve_inside,
};

/// The ending of a song folder's name.
pub const SONG_FOLDER_EXTENSION: &str = ".song";

/// Longest folder name, in bytes, before " 2" and the extension.
const MAX_FOLDER_NAME_BYTES: usize = 200;

/// Name an empty title gets.
const UNTITLED: &str = "Untitled";

/// Bytes copied at a time, between two progress reports.
const COPY_CHUNK: usize = 1 << 20;

/// The folder name, without [`SONG_FOLDER_EXTENSION`], that a song titled
/// `title` gets before a number makes it unique: see the
/// [import docs](super#saving).
pub fn song_folder_name(title: &str) -> String {
    let mut name = String::new();
    for c in title.chars() {
        let piece = match c {
            '/' | '\\' | ':' => '-',
            '*' | '?' | '"' | '<' | '>' | '|' => continue,
            c if c.is_control() => continue,
            c => c,
        };
        if name.len() + piece.len_utf8() > MAX_FOLDER_NAME_BYTES {
            break;
        }
        name.push(piece);
    }
    let mut name = name.trim_matches(['.', ' ']).to_owned();
    let base_len = name.find('.').unwrap_or(name.len());
    let base = name[..base_len].trim_end_matches(' ');
    if crate::setlists::is_windows_reserved(&name) {
        name.insert(base.len(), '_');
    }
    if name.is_empty() {
        UNTITLED.to_owned()
    } else {
        name
    }
}

/// A message from a worker writing stems.
enum Message {
    /// Frames of stem `stem` written so far.
    Progress { stem: usize, done: u64 },
    /// Stem `stem` is written and read back, with its length in frames, or
    /// why not.
    Done {
        stem: usize,
        result: Result<u64, ImportError>,
    },
}

impl Library {
    /// Saves `draft` as a new song folder in the library folder and returns
    /// it as [`Library::list_songs`] lists it; see the
    /// [import docs](super#saving).
    ///
    /// It runs on the caller's thread, which it blocks until the song is
    /// saved, and writes the stems on up to as many threads as the machine
    /// runs at once. `progress` is called on the caller's thread as stems are
    /// written: per MiB copied and about every second of audio transcoded.
    /// When `cancel` becomes `true`, the save stops within about a second of
    /// audio and returns [`ImportError::Cancelled`].
    ///
    /// The draft must have no [refused files](ImportDraft::refused) and pass
    /// [`domain::SongDraft::to_package`]: else nothing is written and
    /// [`ImportError::Refused`] or [`ImportError::Draft`] lists every
    /// problem. Its [`ImportDraft::song`] must be the one the probe made,
    /// edited: another one is [`ImportError::StemsChanged`]. On any error the
    /// library folder is left as it was.
    pub fn save_import(
        &self,
        draft: &ImportDraft,
        progress: &mut dyn FnMut(ImportProgress),
        cancel: &AtomicBool,
    ) -> Result<SongEntry, ImportError> {
        let matches = draft.song.stems().len() == draft.stems.len()
            && draft
                .song
                .stems()
                .iter()
                .zip(&draft.stems)
                .all(|(facts, stem)| facts.source_name == stem.name);
        if !matches {
            return Err(ImportError::StemsChanged);
        }
        if !draft.refused.is_empty() {
            return Err(ImportError::Refused(draft.refused.clone()));
        }
        let packaged = draft.song.to_package().map_err(ImportError::Draft)?;
        let root = self.canonical_root()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(ImportError::Cancelled);
        }
        let scratch = Scratch::create_in(&root).map_err(|source| ImportError::Write {
            path: root.clone(),
            source,
        })?;
        let stems_folder = scratch.path.join(domain::STEMS_FOLDER);
        fs::create_dir(&stems_folder).map_err(|source| ImportError::Write {
            path: stems_folder.clone(),
            source,
        })?;

        let lengths = write_stems(draft, &packaged, &scratch.path, progress, cancel)?;
        write_manifest(&scratch.path, &packaged)?;
        let song = check(&scratch.path, draft, &packaged, &lengths)?;
        sync_folder(&stems_folder);
        sync_folder(&scratch.path);
        if cancel.load(Ordering::Relaxed) {
            return Err(ImportError::Cancelled);
        }

        let name = free_folder_name(&root, &song_folder_name(&packaged.package.title))?;
        let target = root.join(&name);
        fs::rename(&scratch.path, &target).map_err(|source| ImportError::Write {
            path: target.clone(),
            source,
        })?;
        scratch.keep();
        sync_folder(&root);
        Ok(entry(&name, &song))
    }
}

/// Writes every stem of `draft` into `folder`'s stems folder, and reads each
/// one back to its end with the player's reader, on scoped threads,
/// reporting progress on this thread. Returns each stem's length as
/// written: the probe's, except for a stem whose length was an estimate.
fn write_stems(
    draft: &ImportDraft,
    packaged: &PackagedDraft,
    folder: &Path,
    progress: &mut dyn FnMut(ImportProgress),
    cancel: &AtomicBool,
) -> Result<Vec<u64>, ImportError> {
    let lengths: Vec<u64> = draft
        .song
        .stems()
        .iter()
        .map(|stem| stem.length_frames)
        .collect();
    let total = lengths.iter().sum();
    let workers = thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(draft.stems.len())
        .max(1);
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let (next, stop) = (&next, &stop);
            scope.spawn(move || {
                let mut archive = None;
                loop {
                    let stem = next.fetch_add(1, Ordering::Relaxed);
                    if stem >= draft.stems.len()
                        || stop.load(Ordering::Relaxed)
                        || cancel.load(Ordering::Relaxed)
                    {
                        return;
                    }
                    let written = &packaged.package.stems[stem];
                    let target = folder.join(&written.file);
                    let going =
                        || !(stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed));
                    let mut report = |done| {
                        let _ = sender.send(Message::Progress { stem, done });
                        going()
                    };
                    let result =
                        write_stem(draft, stem, folder, &target, &mut archive, &mut report)
                            .and_then(|frames| {
                                let rate = draft.song.stems()[stem].sample_rate;
                                let probed = &draft.stems[stem];
                                verify_stem(folder, written, probed, rate, frames, &mut || going())
                                    .map(|()| frames)
                            });
                    if result.is_err() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    let _ = sender.send(Message::Done { stem, result });
                }
            });
        }
        drop(sender);

        let mut done = vec![0; lengths.len()];
        let mut written = lengths.clone();
        let mut first_error = None;
        for message in receiver {
            let (stem, frames) = match message {
                Message::Progress { stem, done } => (stem, done),
                Message::Done { stem, result } => match result {
                    Ok(frames) => {
                        written[stem] = frames;
                        (stem, lengths[stem])
                    }
                    Err(error) => {
                        // A cancel shows as a stopped stem: report the
                        // first real error, else the cancel.
                        if first_error.is_none()
                            || matches!(first_error, Some(ImportError::Cancelled))
                        {
                            first_error = Some(error);
                        }
                        continue;
                    }
                },
            };
            done[stem] = frames.min(lengths[stem]);
            if first_error.is_none() {
                progress(ImportProgress {
                    stem,
                    stem_done: done[stem],
                    stem_frames: lengths[stem],
                    done: done.iter().sum(),
                    total,
                });
            }
            if cancel.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None if cancel.load(Ordering::Relaxed) => Err(ImportError::Cancelled),
            None => Ok(written),
        }
    })
}

/// Copies or transcodes stem `index` of `draft` to `target`, and returns
/// the frames written. `report` gets the frames written so far and says
/// whether to go on.
fn write_stem(
    draft: &ImportDraft,
    index: usize,
    folder: &Path,
    target: &Path,
    archive: &mut Option<zip::ZipArchive<File>>,
    report: &mut dyn FnMut(u64) -> bool,
) -> Result<u64, ImportError> {
    let stem = &draft.stems[index];
    let facts = &draft.song.stems()[index];
    let stem_error = |reason: String| ImportError::Stem {
        name: stem.name.clone(),
        reason,
    };
    let write_error = |source| ImportError::Write {
        path: target.to_owned(),
        source,
    };
    // A ZIP entry is extracted first, under a made-up name in the hidden
    // folder, and removed when done with.
    let (input, extracted) = match (&draft.source, &stem.location) {
        (Source::Folder(root), Location::File(file)) => (open_source(root, file, stem)?, None),
        (Source::Zip(path), Location::Entry(entry)) => {
            if archive.is_none() {
                *archive = Some(source::open_zip(path)?);
            }
            let zip = archive.as_mut().expect("opened above");
            check_entry_name(zip, *entry, stem)?;
            let name = source::extract(zip, *entry, folder).map_err(stem_error)?;
            (folder.join(&name), Some(folder.join(name)))
        }
        _ => unreachable!("a draft's locations match its source"),
    };
    let result = match &stem.action {
        StemAction::Copy => match &extracted {
            Some(file) => fs::rename(file, target).map_err(write_error).map(|()| {
                report(facts.length_frames);
                facts.length_frames
            }),
            None => copy(&input, target, facts.length_frames, report)
                .map_err(write_error)
                .and_then(|copied| {
                    if copied {
                        Ok(facts.length_frames)
                    } else {
                        Err(ImportError::Cancelled)
                    }
                }),
        },
        StemAction::Transcode { .. } => {
            let audio = AudioFacts {
                sample_rate: facts.sample_rate,
                channels: stem.channels,
                frames: facts.length_frames,
                skip: stem.skip_frames,
                estimated: stem.estimated_length,
            };
            audio::transcode(&input, stem.format, audio, target, report).map_err(
                |error| match error {
                    TranscodeError::Stopped => ImportError::Cancelled,
                    TranscodeError::Source(reason) => stem_error(reason),
                    TranscodeError::Write(source) => write_error(source),
                },
            )
        }
    };
    if let Some(file) = extracted {
        let _ = fs::remove_file(file);
    }
    result
}

/// The path of the source file `file` inside the source folder `root`, once
/// it is checked to resolve inside it and to be a regular file.
fn open_source(root: &Path, file: &str, stem: &ProbedStem) -> Result<PathBuf, ImportError> {
    let unreadable = |reason: String| ImportError::Stem {
        name: stem.name.clone(),
        reason,
    };
    match resolve_inside(root, file) {
        Ok(Some(path)) => Ok(path),
        Ok(None) => Err(unreadable(
            "it resolves outside the source folder".to_owned(),
        )),
        Err(error) => Err(unreadable(error.to_string())),
    }
}

/// Checks that ZIP entry `entry` is still the file the draft names.
fn check_entry_name(
    archive: &mut zip::ZipArchive<File>,
    entry: usize,
    stem: &ProbedStem,
) -> Result<(), ImportError> {
    let same = archive
        .by_index_raw(entry)
        .ok()
        .and_then(|file| file.name().ok().map(|name| name.replace('\\', "/")))
        .is_some_and(|name| name == stem.name);
    if same {
        Ok(())
    } else {
        Err(ImportError::Stem {
            name: stem.name.clone(),
            reason: "the ZIP file changed since it was read".to_owned(),
        })
    }
}

/// Copies the regular file at `from` to a new file at `to`, flushed to disk,
/// reporting progress in frames of a stem of `frames` frames; `false` when
/// `report` asked to stop.
fn copy(
    from: &Path,
    to: &Path,
    frames: u64,
    report: &mut dyn FnMut(u64) -> bool,
) -> io::Result<bool> {
    let (mut input, metadata) =
        open_regular_file(from).map_err(|error| io::Error::other(error.to_string()))?;
    let bytes = metadata.len().max(1);
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut buffer = vec![0; COPY_CHUNK];
    let mut copied = 0u64;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        copied += read as u64;
        let done = (u128::from(copied) * u128::from(frames) / u128::from(bytes)) as u64;
        if !report(done.min(frames)) {
            return Ok(false);
        }
    }
    output.sync_all()?;
    Ok(true)
}

/// Writes `song.json` into `folder`, flushed to disk.
fn write_manifest(folder: &Path, packaged: &PackagedDraft) -> Result<(), ImportError> {
    let path = folder.join(MANIFEST_FILE);
    let json = packaged
        .package
        .to_json()
        .map_err(|error| ImportError::Write {
            path: path.clone(),
            source: io::Error::other(error.to_string()),
        })?;
    let write = || -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    };
    write().map_err(|source| ImportError::Write {
        path: path.clone(),
        source,
    })
}

/// Opens the stem `stem` written in `folder` (canonical) with the player's
/// reader, checks its sample rate and length against what the probe read
/// (`sample_rate`) and what was written (`frames`), and reads it through to
/// its end, a block at a time. `keep_going` is asked between blocks.
fn verify_stem(
    folder: &Path,
    stem: &Stem,
    probed: &ProbedStem,
    sample_rate: u32,
    frames: u64,
    keep_going: &mut dyn FnMut() -> bool,
) -> Result<(), ImportError> {
    let failed = |reason: String| ImportError::Stem {
        name: probed.name.clone(),
        reason,
    };
    let opened = match open_stem(folder, stem) {
        Ok(Some(opened)) => opened,
        Ok(None) => unreachable!("an import writes stems at plain relative paths"),
        Err(problem) => {
            return Err(failed(format!(
                "the stem written cannot be opened: {problem}"
            )));
        }
    };
    if opened.sample_rate() != sample_rate || opened.frames() != frames {
        return Err(failed(format!(
            "the stem written is {} frames at {} Hz, but the file read was {frames} frames at {sample_rate} Hz",
            opened.frames(),
            opened.sample_rate(),
        )));
    }
    match read_through(stem, opened, keep_going) {
        Ok(true) => Ok(()),
        Ok(false) => Err(ImportError::Cancelled),
        Err(error) => Err(failed(format!(
            "the stem written does not read to its end: {error}"
        ))),
    }
}

/// Opens the song folder at `folder` as the player does, checks every
/// stem's sample rate and length against the probe's rate and the lengths
/// written, and returns the song as it loads. A failure names the stem it
/// is about, with every problem found.
fn check(
    folder: &Path,
    draft: &ImportDraft,
    packaged: &PackagedDraft,
    lengths: &[u64],
) -> Result<domain::Song, ImportError> {
    let (validated, opened) =
        open_package(folder).map_err(|error| load_failure(error, draft, packaged))?;
    for (((stem, facts), (_, opened)), &frames) in draft
        .stems
        .iter()
        .zip(draft.song.stems())
        .zip(&opened)
        .zip(lengths)
    {
        if opened.sample_rate() != facts.sample_rate || opened.frames() != frames {
            return Err(ImportError::Stem {
                name: stem.name.clone(),
                reason: format!(
                    "the stem written is {} frames at {} Hz, but the file read was {frames} frames at {} Hz",
                    opened.frames(),
                    opened.sample_rate(),
                    facts.sample_rate
                ),
            });
        }
    }
    Ok(validated.song)
}

/// A song written that does not load, as the error of the stem it is about
/// (the source when it is about no stem), with the loader's every problem.
fn load_failure(error: LoadError, draft: &ImportDraft, packaged: &PackagedDraft) -> ImportError {
    let (id, detail) = match &error {
        LoadError::Invalid(problems) => (
            problems.iter().find_map(problem_stem),
            problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        ),
        LoadError::Decode { id, .. } | LoadError::StemTooLarge { id, .. } => {
            (Some(id.as_str()), error.to_string())
        }
        other => (None, other.to_string()),
    };
    let name = id
        .and_then(|id| packaged.package.stems.iter().position(|stem| stem.id == id))
        .map_or_else(
            || draft.source().display().to_string(),
            |index| draft.stems[index].name.clone(),
        );
    ImportError::Stem {
        name,
        reason: format!("the song written does not load: {detail}"),
    }
}

/// `name` with [`SONG_FOLDER_EXTENSION`], or with " 2", " 3"… before it,
/// the first that names nothing in `root`.
fn free_folder_name(root: &Path, name: &str) -> Result<String, ImportError> {
    for n in 1u64.. {
        let candidate = match n {
            1 => format!("{name}{SONG_FOLDER_EXTENSION}"),
            n => format!("{name} {n}{SONG_FOLDER_EXTENSION}"),
        };
        match fs::symlink_metadata(root.join(&candidate)) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(source) => {
                return Err(ImportError::Write {
                    path: root.join(candidate),
                    source,
                });
            }
        }
    }
    unreachable!("some numbered name is free")
}

/// Flushes a folder's entries to disk where the platform allows it.
fn sync_folder(folder: &Path) {
    if let Ok(folder) = File::open(folder) {
        let _ = folder.sync_all();
    }
}
