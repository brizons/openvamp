//! Reading a source: the audio files of a folder or a ZIP file, the facts of
//! each one, and what the import does with it.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use domain::{DraftStem, SongDraft, Stem, StemRole, StoredFormat};
use zip::ZipArchive;

use super::audio;
use super::{
    ImportDraft, ImportError, Location, MAX_IMPORT_FILES, ProbedStem, RefusedFile, Source,
    SourceFormat, StemAction,
};
use crate::{OpenedStem, SongProblem, open_stem, resolve_inside};

/// Deepest folder level a folder import looks into.
const MAX_DEPTH: usize = 32;

/// Largest ZIP entry extracted, in bytes: a WAV file holds at most 4 GiB.
const MAX_ENTRY_BYTES: u64 = 1 << 32;

/// Where a stem is copied or transcoded from, as [`super::save`] reads it.
pub(super) struct Facts {
    pub(super) sample_rate: u32,
    pub(super) channels: u16,
    pub(super) frames: u64,
    pub(super) skip: u64,
    pub(super) stored_as: StoredFormat,
    pub(super) action: StemAction,
}

/// [`super::probe_import`].
pub(super) fn probe(path: &Path) -> Result<ImportDraft, ImportError> {
    let source_error = |source| ImportError::Source {
        path: path.to_owned(),
        source,
    };
    let metadata = fs::metadata(path).map_err(source_error)?;
    let canonical = fs::canonicalize(path).map_err(source_error)?;
    let is_zip = metadata.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    let (source, title, probed) = if metadata.is_dir() {
        let title = canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let probed = probe_folder(&canonical)?;
        (Source::Folder(canonical), title, probed)
    } else if is_zip {
        let title = canonical
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let probed = probe_zip(&canonical)?;
        (Source::Zip(canonical), title, probed)
    } else {
        return Err(ImportError::NotFolderOrZip {
            path: path.to_owned(),
        });
    };

    let mut stems = Vec::new();
    let mut draft_stems = Vec::new();
    let mut refused = Vec::new();
    for (name, format, location, facts) in probed {
        match facts {
            Ok(facts) => {
                draft_stems.push(DraftStem::new(
                    name.clone(),
                    facts.stored_as,
                    facts.sample_rate,
                    facts.frames,
                ));
                stems.push(ProbedStem {
                    name,
                    format,
                    channels: facts.channels,
                    action: facts.action,
                    location,
                    skip_frames: facts.skip,
                });
            }
            Err(reason) => refused.push(RefusedFile { name, reason }),
        }
    }
    Ok(ImportDraft {
        song: SongDraft::new(title, draft_stems),
        stems,
        refused,
        source,
    })
}

/// One audio file found: its name in the source, the format its extension
/// says, where it is, and its facts or why it is refused.
type Probed = (String, SourceFormat, Location, Result<Facts, String>);

/// Probes every audio file of the folder `root` (canonical).
fn probe_folder(root: &Path) -> Result<Vec<Probed>, ImportError> {
    let mut files = Vec::new();
    walk(root, "", 0, &mut files).map_err(|source| ImportError::Source {
        path: root.to_owned(),
        source,
    })?;
    check_count(root, files.len())?;
    sort_names(&mut files);
    Ok(files
        .into_iter()
        .map(|(name, format)| {
            let facts = probe_file(root, &name, format);
            (name.clone(), format, Location::File(name), facts)
        })
        .collect())
}

/// Adds the audio files of `folder`, at `prefix` inside the source, and of
/// the folders inside it, to `files`; it stops once there are more than an
/// import takes.
fn walk(
    folder: &Path,
    prefix: &str,
    depth: usize,
    files: &mut Vec<(String, SourceFormat)>,
) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if is_skipped(&name) {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                walk(&entry.path(), &path, depth + 1, files)?;
            }
        } else if let Some(format) = SourceFormat::from_name(&name) {
            files.push((path, format));
        }
        if files.len() > MAX_IMPORT_FILES {
            return Ok(());
        }
    }
    Ok(())
}

/// Whether a file or folder name is left out: hidden, or the `__MACOSX`
/// folder ZIP files made on macOS hold.
fn is_skipped(name: &str) -> bool {
    (name.starts_with('.') && name != "." && name != "..") || name == "__MACOSX"
}

fn check_count(path: &Path, count: usize) -> Result<(), ImportError> {
    if count == 0 {
        return Err(ImportError::NoAudio {
            path: path.to_owned(),
        });
    }
    if count > MAX_IMPORT_FILES {
        return Err(ImportError::TooManyFiles {
            path: path.to_owned(),
            max: MAX_IMPORT_FILES,
        });
    }
    Ok(())
}

/// Sorts names by path ignoring case, then exactly.
fn sort_names<T>(files: &mut [(String, T)]) {
    files.sort_by(|(a, _), (b, _)| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
}

/// Probes every audio entry of the ZIP file at `path` (canonical), each
/// extracted in turn to a hidden folder in the system's temporary folder.
fn probe_zip(path: &Path) -> Result<Vec<Probed>, ImportError> {
    let mut archive = open_zip(path)?;
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|error| zip_error(path, error))?;
        let Ok(name) = entry.name() else {
            continue;
        };
        let name = name.replace('\\', "/");
        if entry.is_dir() || name.split('/').any(is_skipped) {
            continue;
        }
        if let Some(format) = SourceFormat::from_name(&name) {
            entries.push((name, (format, index)));
        }
    }
    check_count(path, entries.len())?;
    sort_names(&mut entries);
    let temp_error = |source| ImportError::Source {
        path: std::env::temp_dir(),
        source,
    };
    // The player's reader wants a canonical folder; macOS links /var.
    let temp = fs::canonicalize(std::env::temp_dir()).map_err(temp_error)?;
    let scratch = Scratch::create_in(&temp).map_err(temp_error)?;
    Ok(entries
        .into_iter()
        .map(|(name, (format, index))| {
            let facts = extract(&mut archive, index, &scratch.path).and_then(|file| {
                let facts = probe_file(&scratch.path, &file, format);
                let _ = fs::remove_file(scratch.path.join(&file));
                facts
            });
            (name, format, Location::Entry(index), facts)
        })
        .collect())
}

pub(super) fn open_zip(path: &Path) -> Result<ZipArchive<File>, ImportError> {
    let file = File::open(path).map_err(|source| ImportError::Source {
        path: path.to_owned(),
        source,
    })?;
    ZipArchive::new(file).map_err(|error| zip_error(path, error))
}

fn zip_error(path: &Path, error: zip::result::ZipError) -> ImportError {
    ImportError::Zip {
        path: path.to_owned(),
        reason: error.to_string(),
    }
}

/// Extracts ZIP entry `index` into `folder` under a name made up here, never
/// the entry's own; returns that name. A failure is the reason the entry is
/// refused, and leaves no file.
pub(super) fn extract(
    archive: &mut ZipArchive<File>,
    index: usize,
    folder: &Path,
) -> Result<String, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(".entry-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let path = folder.join(&name);
    let result = (|| {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("it cannot be read from the ZIP file: {error}"))?;
        if entry.size() > MAX_ENTRY_BYTES {
            return Err("it is larger than any stem (4 GiB)".to_owned());
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("it cannot be extracted: {error}"))?;
        let copied = io::copy(&mut (&mut entry).take(MAX_ENTRY_BYTES + 1), &mut file)
            .map_err(|error| format!("it cannot be extracted: {error}"))?;
        if copied > MAX_ENTRY_BYTES {
            return Err("it is larger than any stem (4 GiB)".to_owned());
        }
        file.flush()
            .map_err(|error| format!("it cannot be extracted: {error}"))?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(name),
        Err(reason) => {
            let _ = fs::remove_file(&path);
            Err(reason)
        }
    }
}

/// The facts of the file `file` inside `folder` (canonical), read as the
/// player reads a stem when it is WAV or FLAC, else with the transcoder; or
/// why the import refuses it.
pub(super) fn probe_file(folder: &Path, file: &str, format: SourceFormat) -> Result<Facts, String> {
    let player_problem = match format {
        SourceFormat::Wav | SourceFormat::Flac => {
            let stem = Stem {
                id: "import".to_owned(),
                file: file.to_owned(),
                part: String::new(),
                role: StemRole::Track,
            };
            match open_stem(folder, &stem) {
                Ok(Some(opened)) => return copied(&opened),
                Ok(None) => return Err("its path is not inside the source".to_owned()),
                Err(SongProblem::StemOutsideFolder { .. }) => {
                    return Err("it resolves outside the source folder".to_owned());
                }
                Err(problem) => Some(player_reason(problem)),
            }
        }
        SourceFormat::Mp3 | SourceFormat::M4a => None,
    };
    let path = match resolve_inside(folder, file) {
        Ok(Some(path)) => path,
        Ok(None) => return Err("it resolves outside the source folder".to_owned()),
        Err(error) => return Err(format!("it cannot be read: {error}")),
    };
    let facts =
        audio::probe(&path, format).map_err(|reason| format!("it cannot be decoded: {reason}"))?;
    if facts.channels > 2 {
        return Err(too_many_channels(facts.channels));
    }
    if facts.frames == 0 {
        return Err("it holds no audio".to_owned());
    }
    let reason = player_problem.unwrap_or_else(|| match format {
        SourceFormat::M4a => "AAC is lossy; it is stored as FLAC so it seeks exactly".to_owned(),
        _ => "MP3 is lossy; it is stored as FLAC so it seeks exactly".to_owned(),
    });
    Ok(Facts {
        sample_rate: facts.sample_rate,
        channels: facts.channels,
        frames: facts.frames,
        skip: facts.skip,
        stored_as: StoredFormat::Flac,
        action: StemAction::Transcode { reason },
    })
}

/// The facts of a stem the player reads as it is, copied as it is.
fn copied(opened: &OpenedStem) -> Result<Facts, String> {
    if opened.frames() == 0 {
        return Err("it holds no audio".to_owned());
    }
    Ok(Facts {
        sample_rate: opened.sample_rate(),
        channels: opened.channels(),
        frames: opened.frames(),
        skip: 0,
        stored_as: match opened {
            OpenedStem::Wav { .. } => StoredFormat::Wav,
            OpenedStem::Flac(_) => StoredFormat::Flac,
        },
        action: StemAction::Copy,
    })
}

fn too_many_channels(channels: u16) -> String {
    format!("it has {channels} channels; stems must be mono or stereo")
}

/// Why the player cannot read a WAV or FLAC file as it is, in words.
fn player_reason(problem: SongProblem) -> String {
    let detail = match problem {
        SongProblem::UnsupportedStemFormat { format, .. }
        | SongProblem::UnsupportedFlacFormat { format, .. } => format,
        SongProblem::StemUnreadable { reason, .. } => reason,
        other => other.to_string(),
    };
    format!("the player cannot read it as it is ({detail})")
}

/// A hidden folder, `.import-<pid>-<n>.tmp`, removed with everything in it
/// on drop unless kept.
pub(super) struct Scratch {
    pub(super) path: PathBuf,
    kept: bool,
}

impl Scratch {
    /// Creates a new hidden folder in `parent` (canonical), never reusing
    /// one that exists.
    pub(super) fn create_in(parent: &Path) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut last_error = None;
        for _ in 0..100 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".import-{}-{n}.tmp", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, kept: false }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("no free temporary folder name")))
    }

    /// Keeps the folder: it was renamed into place.
    pub(super) fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.kept {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
