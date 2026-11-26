//! Song package I/O for the multitrack player.
//!
//! [`load_song`] reads a song folder (`song.json` plus WAV stems), validates it
//! with the `domain` crate, and decodes every stem to `f32` for the engine.
//! [`open_streaming`] checks the same folder but streams the stems from disk
//! for the live player, with memory that does not grow with song length.
//! [`write_wav_stereo_f32`] writes a rendered mix. A [`Library`] keeps the
//! setlists of a library folder as `*.setlist.json` files in its
//! [`SETLISTS_FOLDER`]: it lists, loads, saves, renames and duplicates them,
//! never leaving a half-written file or overwriting another setlist, and
//! keeps the order of the list in its [`SETLIST_ORDER_FILE`]
//! ([`Library::move_setlist`]); a setlist removed from the list
//! ([`Library::remove_setlist`]) goes to the system trash, never deleted.
//! [`Library::setlist_songs`] reads the songs a setlist names, so
//! [`domain::Setlist::validate`] can report the ones the library lacks while
//! the rest of the setlist stays usable. [`Library::list_songs`] lists every
//! song folder in the library, from the manifest and stem headers only.
//! [`read_mix`] and [`write_mix`] keep a song's mix in the [`MIX_FILE`]
//! beside its `song.json`, with the rules the [settings files](SettingsError)
//! share. A [`ConfigStore`] keeps the global bus settings and the routing of
//! every device in the app's configuration folder ([`config_folder`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process;

use domain::{
    ManifestError, Song, SongPackage, Stem, StemInfo, Validated, ValidationError,
    ValidationWarning, validate,
};
use engine::StemAudio;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use thiserror::Error;

mod config;
mod mix_store;
mod setlist_order;
mod setlist_songs;
mod setlists;
mod settings;
mod songs;
mod stream;

pub use config::{
    BUS_SETTINGS_FILE, ConfigStore, LoadedBusSettings, LoadedRouting, Platform, ROUTING_FILE,
    config_folder,
};
pub use mix_store::{MIX_FILE, SongMix, read_mix, write_mix};
pub use setlist_songs::{SetlistSongs, SongUnavailable};
pub use setlists::{
    Library, MAX_SETLIST_FILE_BYTES, SETLIST_EXTENSION, SETLIST_ORDER_FILE,
    SETLIST_ORDER_FORMAT_VERSION, SETLISTS_FOLDER, SetlistEntry, SetlistError, SetlistListing,
    TrashFn, UnreadableSetlist, move_to_trash,
};
pub use settings::{MAX_SETTINGS_FILE_BYTES, SettingsError};
pub use songs::{ArrangementEntry, PositionEntry, SongEntry, SongListing, UnreadableSong};
pub use stream::{DiskMonitor, Gate, StreamCapacity, StreamConfig, StreamingSong, open_streaming};

/// Name of the manifest inside a song folder.
pub const MANIFEST_FILE: &str = "song.json";

/// A validated song with its decoded stems.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSong {
    /// The validated song.
    pub song: Song,
    /// Findings that do not block playback.
    pub warnings: Vec<ValidationWarning>,
    /// Decoded stems, in package order.
    pub stems: Vec<StemAudio>,
}

/// Why a song folder could not be loaded.
#[derive(Debug, Error)]
pub enum LoadError {
    /// `song.json` could not be read.
    #[error("cannot read {}: {source}", path.display())]
    ReadManifest {
        /// Path of the manifest.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// `song.json` is not a valid v1 manifest.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// `song.json` is a folder, FIFO, device or anything but a regular
    /// file; it was not opened, so a FIFO cannot block the reader.
    #[error("{} is not a regular file", path.display())]
    ManifestNotAFile {
        /// Path of the manifest.
        path: PathBuf,
    },
    /// `song.json` was replaced by another file while it was being opened.
    #[error("{} changed while it was being opened", path.display())]
    ManifestChanged {
        /// Path of the manifest.
        path: PathBuf,
    },
    /// The song folder could not be resolved.
    #[error("cannot open song folder {}: {source}", path.display())]
    SongFolder {
        /// The folder.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The package breaks one or more rules; every problem is listed.
    #[error("the song package has {} problem(s)", .0.len())]
    Invalid(Vec<SongProblem>),
    /// A stem's samples do not fit in memory.
    #[error("stem \"{id}\" file \"{file}\" has {samples} samples, more than fit in memory")]
    StemTooLarge {
        /// Stem id.
        id: String,
        /// Stem file, as in the manifest.
        file: String,
        /// Samples to decode.
        samples: u64,
    },
    /// A stem passed validation but its audio could not be decoded.
    #[error("stem \"{id}\" file \"{file}\" cannot be decoded: {source}")]
    Decode {
        /// Stem id.
        id: String,
        /// Stem file, as in the manifest.
        file: String,
        /// The decoder error.
        source: hound::Error,
    },
    /// The thread that streams the stems could not be started.
    #[error("cannot start the disk thread: {source}")]
    DiskThread {
        /// The error from the operating system.
        source: io::Error,
    },
}

/// One problem found while checking a song folder.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SongProblem {
    /// A package rule from [`domain::validate`].
    #[error(transparent)]
    Validation(ValidationError),
    /// The stem file resolves, through symlinks or otherwise, outside the song
    /// folder.
    #[error("stem \"{id}\" file \"{file}\" resolves outside the song folder")]
    StemOutsideFolder {
        /// Stem id.
        id: String,
        /// Stem file, as in the manifest.
        file: String,
    },
    /// The stem file is missing or is not a readable WAV file.
    #[error("stem \"{id}\" file \"{file}\" cannot be read: {reason}")]
    StemUnreadable {
        /// Stem id.
        id: String,
        /// Stem file, as in the manifest.
        file: String,
        /// What went wrong.
        reason: String,
    },
    /// The stem is a WAV file in a format this version does not decode.
    #[error(
        "stem \"{id}\" file \"{file}\" is {format}; supported stems are 16-, 24- or 32-bit integer or 32-bit float WAV, mono or stereo"
    )]
    UnsupportedStemFormat {
        /// Stem id.
        id: String,
        /// Stem file, as in the manifest.
        file: String,
        /// The format found, such as "8-bit integer, 1 channel".
        format: String,
    },
}

/// Why a WAV file could not be written.
#[derive(Debug, Error)]
pub enum WriteError {
    /// The sample count is odd, so it is not interleaved stereo.
    #[error("{samples} samples are not interleaved stereo; the count must be even")]
    NotStereo {
        /// Number of samples given.
        samples: usize,
    },
    /// The audio does not fit in a WAV file, whose sizes are 32-bit.
    #[error(
        "the render is {frames} frames ({}) long, but a WAV file holds at most 4 GiB of audio: {max_frames} frames of stereo 32-bit float, {} at {sample_rate} Hz",
        minutes_seconds(*frames, *sample_rate),
        minutes_seconds(*max_frames, *sample_rate)
    )]
    TooLong {
        /// Frames to write.
        frames: u64,
        /// [`MAX_WAV_STEREO_F32_FRAMES`].
        max_frames: u64,
        /// Sample rate of the audio.
        sample_rate: u32,
    },
    /// The file could not be created or written. An existing file at `path`
    /// is left as it was.
    #[error("cannot write {}: {source}", path.display())]
    Wav {
        /// Output path.
        path: PathBuf,
        /// The encoder error.
        source: hound::Error,
    },
}

/// Reads, validates, and decodes the song folder `dir`.
///
/// Every stem path is resolved inside the song folder; one that leaves it,
/// symlinks included, is rejected before the file is opened. When the folder
/// has problems, every one is returned together in [`LoadError::Invalid`]:
/// stem file problems first, in package order, then the package rules
/// [`domain::validate`] reports. A stem with a file problem is not reported a
/// second time as missing its audio information, and a stem whose path breaks
/// the package rules reports only that rule.
pub fn load_song(dir: &Path) -> Result<LoadedSong, LoadError> {
    let (validated, opened) = open_package(dir)?;
    let stems = opened
        .into_iter()
        .map(|(stem, opened)| decode(&stem, opened))
        .collect::<Result<_, _>>()?;
    Ok(LoadedSong {
        song: validated.song,
        warnings: validated.warnings,
        stems,
    })
}

/// Reads and validates the song folder `dir` as [`load_song`] describes, and
/// opens every stem file, in package order, without decoding it.
fn open_package(dir: &Path) -> Result<(Validated, Vec<(Stem, OpenedStem)>), LoadError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let json = fs::read_to_string(&manifest_path).map_err(|source| LoadError::ReadManifest {
        path: manifest_path,
        source,
    })?;
    open_package_json(dir, &json)
}

/// [`open_package`] for the manifest text `json` already read from `dir`.
fn open_package_json(
    dir: &Path,
    json: &str,
) -> Result<(Validated, Vec<(Stem, OpenedStem)>), LoadError> {
    let package = SongPackage::from_json(json)?;
    let folder = fs::canonicalize(dir).map_err(|source| LoadError::SongFolder {
        path: dir.to_owned(),
        source,
    })?;

    let mut file_problems = Vec::new();
    let mut infos = Vec::with_capacity(package.stems.len());
    let mut readers = Vec::with_capacity(package.stems.len());
    for stem in &package.stems {
        match open_stem(&folder, stem) {
            Ok(Some(opened)) => {
                infos.push(StemInfo {
                    id: stem.id.clone(),
                    sample_rate: opened.reader.spec().sample_rate,
                    length_samples: u64::from(opened.reader.duration()),
                });
                readers.push(Some(opened));
            }
            // `validate` reports the path; nothing was touched on disk.
            Ok(None) => readers.push(None),
            Err(problem) => {
                file_problems.push(problem);
                readers.push(None);
            }
        }
    }

    match validate(&package, &infos) {
        Ok(validated) if file_problems.is_empty() => {
            // Without problems every stem passed its file checks.
            let opened = package
                .stems
                .into_iter()
                .zip(readers)
                .filter_map(|(stem, reader)| reader.map(|reader| (stem, reader)))
                .collect();
            Ok((validated, opened))
        }
        Ok(_) => Err(LoadError::Invalid(file_problems)),
        Err(errors) => Err(LoadError::Invalid(merge_problems(file_problems, errors))),
    }
}

type StemReader = WavReader<BufReader<File>>;

/// A stem file that passed every check, open for decoding.
struct OpenedStem {
    reader: StemReader,
    /// Most samples the file can hold, from its size on disk.
    max_samples: u64,
}

/// Opens the stem's WAV file after these checks, in order:
///
/// 1. Its path is relative with no `..` component, as this platform reads
///    paths. Nothing on disk is touched before this check; a path that fails
///    it returns `None` and [`domain::validate`] reports it.
/// 2. It resolves, symlinks included, inside `folder` (already canonical).
/// 3. It is a regular file, so opening it cannot block (a FIFO would).
///
/// Then the file is opened and its format checked.
fn open_stem(folder: &Path, stem: &Stem) -> Result<Option<OpenedStem>, SongProblem> {
    if !is_plain_relative(&stem.file) {
        return Ok(None);
    }
    let unreadable = |reason: String| SongProblem::StemUnreadable {
        id: stem.id.clone(),
        file: stem.file.clone(),
        reason,
    };
    let resolved = match resolve_inside(folder, &stem.file) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            return Err(SongProblem::StemOutsideFolder {
                id: stem.id.clone(),
                file: stem.file.clone(),
            });
        }
        Err(error) => return Err(unreadable(error.to_string())),
    };
    let (file, opened) =
        open_regular_file(&resolved).map_err(|error| unreadable(error.to_string()))?;
    let reader =
        WavReader::new(BufReader::new(file)).map_err(|error| unreadable(error.to_string()))?;
    let spec = reader.spec();
    let supported = matches!(
        (spec.sample_format, spec.bits_per_sample),
        (SampleFormat::Int, 16 | 24 | 32) | (SampleFormat::Float, 32)
    ) && matches!(spec.channels, 1 | 2);
    if !supported {
        return Err(SongProblem::UnsupportedStemFormat {
            id: stem.id.clone(),
            file: stem.file.clone(),
            format: describe(spec),
        });
    }
    let bytes_per_sample = u64::from(spec.bits_per_sample.div_ceil(8));
    Ok(Some(OpenedStem {
        reader,
        max_samples: opened.len() / bytes_per_sample,
    }))
}

/// Resolves `relative` inside `folder`, which must already be canonical,
/// following symlinks. Returns `None` when the result lands outside `folder`;
/// the caller has already checked that `relative` is a plain relative path.
fn resolve_inside(folder: &Path, relative: impl AsRef<Path>) -> io::Result<Option<PathBuf>> {
    let resolved = fs::canonicalize(folder.join(relative))?;
    Ok(resolved.starts_with(folder).then_some(resolved))
}

/// Why [`open_regular_file`] did not open a file.
#[derive(Debug)]
enum OpenFileError {
    /// The file could not be inspected or opened.
    Io(io::Error),
    /// The path is a folder, FIFO, device or anything but a regular file.
    NotRegular,
    /// The opened file is not the one that was checked.
    Changed,
}

impl fmt::Display for OpenFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::NotRegular => f.write_str("it is not a regular file"),
            Self::Changed => f.write_str("it changed while it was being opened"),
        }
    }
}

/// Opens `path` (already resolved) for reading, only when it is a regular
/// file, so opening it cannot block (a FIFO would). Returns the file and its
/// metadata.
fn open_regular_file(path: &Path) -> Result<(File, fs::Metadata), OpenFileError> {
    let checked = fs::metadata(path).map_err(OpenFileError::Io)?;
    if !checked.is_file() {
        return Err(OpenFileError::NotRegular);
    }
    // Remaining window: a folder entry on `path` could be swapped for a
    // symlink between the checks above and this open. The standard library
    // cannot open a path relative to a folder handle without following
    // symlinks (that needs `openat2` with `RESOLVE_BENEATH` or similar), so a
    // swap to a FIFO can still block here. A swap to another file is caught
    // below by comparing the opened file with the checked one.
    let file = File::open(path).map_err(OpenFileError::Io)?;
    let opened = file.metadata().map_err(OpenFileError::Io)?;
    if !opened.is_file() || !same_file(&checked, &opened) {
        return Err(OpenFileError::Changed);
    }
    Ok((file, opened))
}

/// Why [`read_confined`] did not read a file.
#[derive(Debug)]
enum ConfinedReadError {
    /// The file resolves, through symlinks or otherwise, outside the folder.
    Outside,
    /// The file is a folder, FIFO, device or anything but a regular file; it
    /// was not opened.
    NotAFile,
    /// The file holds more bytes than the limit.
    TooLarge,
    /// The file was read but is not UTF-8 text.
    NotUtf8,
    /// The file could not be resolved, opened or read as UTF-8.
    Io(io::Error),
}

/// Reads the file `file` inside `folder` (canonical) as text: it must
/// resolve inside `folder`, symlinks included, be a regular file (a FIFO is
/// refused without being opened) and hold at most `max_bytes`. Setlists, the
/// setlist order, `mix.json` and the configuration files are all read this
/// way.
fn read_confined(folder: &Path, file: &str, max_bytes: u64) -> Result<String, ConfinedReadError> {
    let resolved = match resolve_inside(folder, file) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return Err(ConfinedReadError::Outside),
        Err(error) => return Err(ConfinedReadError::Io(error)),
    };
    let (opened, metadata) = open_regular_file(&resolved).map_err(|error| match error {
        OpenFileError::NotRegular => ConfinedReadError::NotAFile,
        OpenFileError::Io(error) => ConfinedReadError::Io(error),
        changed @ OpenFileError::Changed => {
            ConfinedReadError::Io(io::Error::other(changed.to_string()))
        }
    })?;
    if metadata.len() > max_bytes {
        return Err(ConfinedReadError::TooLarge);
    }
    let bytes = read_at_most(opened, max_bytes)
        .map_err(ConfinedReadError::Io)?
        .ok_or(ConfinedReadError::TooLarge)?;
    String::from_utf8(bytes).map_err(|_| ConfinedReadError::NotUtf8)
}

/// Reads all of `file`, or `None` when it holds more than `max_bytes` (it may
/// grow after its size was checked).
fn read_at_most(file: File, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= max_bytes).then_some(bytes))
}

/// Writes `contents` to a temporary file beside `path`, flushes it to disk,
/// and renames it over `path`; on failure the temporary file is removed and
/// an existing file at `path` is left as it was. A symlink at `path` is
/// replaced, never followed.
fn write_file_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let (temporary, mut file) = Temporary::create_beside(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    temporary.rename_to(path)
}

/// Whether `file` is a non-empty relative path whose components are all plain
/// names (or `.`): no root, drive, or `..`, as this platform reads it. The
/// cross-platform rule, with `\` as a separator everywhere, is
/// [`domain::validate`]'s.
fn is_plain_relative(file: &str) -> bool {
    !file.is_empty()
        && Path::new(file)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Whether two metadata snapshots describe the same file.
#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Whether two metadata snapshots describe the same file; without file
/// identities in the standard library, compare size and modification time.
#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// A WAV format in words, such as "8-bit integer, 1 channel".
fn describe(spec: WavSpec) -> String {
    let format = match spec.sample_format {
        SampleFormat::Int => "integer",
        SampleFormat::Float => "float",
    };
    let channels = match spec.channels {
        1 => "1 channel".to_owned(),
        n => format!("{n} channels"),
    };
    format!("{}-bit {format}, {channels}", spec.bits_per_sample)
}

/// Combines file problems with validation errors without saying the same
/// thing twice about one stem.
fn merge_problems(
    file_problems: Vec<SongProblem>,
    errors: Vec<ValidationError>,
) -> Vec<SongProblem> {
    let invalid_paths: HashSet<String> = errors
        .iter()
        .filter_map(|error| match error {
            ValidationError::InvalidStemPath { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let file_problems: Vec<SongProblem> = file_problems
        .into_iter()
        .filter(|problem| !problem_stem(problem).is_some_and(|id| invalid_paths.contains(id)))
        .collect();
    let explained: HashSet<String> = file_problems
        .iter()
        .filter_map(problem_stem)
        .chain(invalid_paths.iter().map(String::as_str))
        .map(str::to_owned)
        .collect();
    let rules = errors.into_iter().filter(|error| {
        !matches!(error, ValidationError::MissingStemInfo { id, .. } if explained.contains(id))
    });
    file_problems
        .into_iter()
        .chain(rules.map(SongProblem::Validation))
        .collect()
}

/// Id of the stem a file problem is about.
fn problem_stem(problem: &SongProblem) -> Option<&str> {
    match problem {
        SongProblem::StemOutsideFolder { id, .. }
        | SongProblem::StemUnreadable { id, .. }
        | SongProblem::UnsupportedStemFormat { id, .. } => Some(id),
        SongProblem::Validation(_) => None,
    }
}

/// Decodes a supported WAV stem to interleaved `f32`: integer samples are
/// divided by `2^(bits - 1)`, float samples are kept as they are.
fn decode(stem: &Stem, opened: OpenedStem) -> Result<StemAudio, LoadError> {
    let OpenedStem {
        mut reader,
        max_samples,
    } = opened;
    let spec = reader.spec();
    let frames = u64::from(reader.duration());
    let decode_error = |source| LoadError::Decode {
        id: stem.id.clone(),
        file: stem.file.clone(),
        source,
    };
    // A header can claim more samples than its file holds: reserve no more
    // than the file can contain, and fail cleanly if even that does not fit.
    let capacity = u64::from(reader.len()).min(max_samples);
    let mut samples = Vec::new();
    usize::try_from(capacity)
        .ok()
        .and_then(|capacity| samples.try_reserve_exact(capacity).ok())
        .ok_or_else(|| LoadError::StemTooLarge {
            id: stem.id.clone(),
            file: stem.file.clone(),
            samples: capacity,
        })?;
    match spec.sample_format {
        SampleFormat::Int => {
            let scale = int_scale(spec.bits_per_sample);
            for sample in reader.samples::<i32>() {
                samples.push(sample.map_err(decode_error)? as f32 * scale);
            }
        }
        SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                samples.push(sample.map_err(decode_error)?);
            }
        }
    }
    Ok(StemAudio {
        id: stem.id.clone(),
        role: stem.role,
        channels: spec.channels,
        samples,
        frames,
    })
}

/// The factor that turns a `bits`-bit integer sample into `f32`: 2^-(bits - 1),
/// exact in f32, so the conversion adds no rounding. Loading and streaming
/// share it, so both give bit-identical samples.
fn int_scale(bits: u16) -> f32 {
    1.0 / (1u64 << (bits - 1)) as f32
}

/// Bytes before the samples in the files [`write_wav_stereo_f32`] writes: the
/// RIFF header, a `WAVE_FORMAT_EXTENSIBLE` fmt chunk, and the data chunk header.
const WAV_HEADER_BYTES: u64 = 68;

/// Bytes per stereo 32-bit float frame.
const STEREO_F32_FRAME_BYTES: u64 = 8;

/// Most frames a stereo 32-bit float WAV file can hold. The RIFF chunk size
/// (the file size minus 8 bytes) is stored in 32 bits, so header and samples
/// together must stay within 4 GiB.
pub const MAX_WAV_STEREO_F32_FRAMES: u64 =
    (u32::MAX as u64 - (WAV_HEADER_BYTES - 8)) / STEREO_F32_FRAME_BYTES;

/// Checks that `frames` of stereo 32-bit float audio fit in a WAV file, so a
/// render can be rejected before any audio is produced.
pub fn check_wav_stereo_f32_frames(frames: u64, sample_rate: u32) -> Result<(), WriteError> {
    if frames > MAX_WAV_STEREO_F32_FRAMES {
        return Err(WriteError::TooLong {
            frames,
            max_frames: MAX_WAV_STEREO_F32_FRAMES,
            sample_rate,
        });
    }
    Ok(())
}

/// `frames` at `sample_rate` as `m:ss`, rounded down.
fn minutes_seconds(frames: u64, sample_rate: u32) -> String {
    let seconds = frames / u64::from(sample_rate.max(1));
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Writes interleaved stereo `samples` as a 32-bit float WAV file.
///
/// The samples go to a temporary file in the same folder, which is flushed to
/// disk and then renamed over `path`. If anything fails, the temporary file is
/// removed and an existing file at `path` is left byte-identical.
pub fn write_wav_stereo_f32(
    path: &Path,
    sample_rate: u32,
    samples: &[f32],
) -> Result<(), WriteError> {
    if !samples.len().is_multiple_of(2) {
        return Err(WriteError::NotStereo {
            samples: samples.len(),
        });
    }
    check_wav_stereo_f32_frames(samples.len() as u64 / 2, sample_rate)?;
    let wav_error = |source| WriteError::Wav {
        path: path.to_owned(),
        source,
    };
    let (temporary, file) = Temporary::create_beside(path)
        .map_err(hound::Error::IoError)
        .map_err(wav_error)?;
    write_samples(file, sample_rate, samples).map_err(wav_error)?;
    temporary
        .rename_to(path)
        .map_err(hound::Error::IoError)
        .map_err(wav_error)
}

/// Writes the WAV file, flushes it to disk, and closes it.
fn write_samples(mut file: File, sample_rate: u32, samples: &[f32]) -> hound::Result<()> {
    let spec = WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::new(BufWriter::new(&mut file), spec)?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    file.sync_all()?;
    Ok(())
}

/// The path of a file being written next to its final path; the file is
/// removed on drop unless it was renamed into place.
struct Temporary {
    path: PathBuf,
    renamed: bool,
}

impl Temporary {
    /// Creates `.<name>.<pid>-<n>.tmp` in the folder of `target`, never
    /// reusing an existing file.
    fn create_beside(target: &Path) -> io::Result<(Self, File)> {
        let name = target.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the path has no file name")
        })?;
        let folder = match target.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let mut last_error = None;
        for attempt in 0..100 {
            let mut temporary_name = OsString::from(".");
            temporary_name.push(name);
            temporary_name.push(format!(".{}-{attempt}.tmp", process::id()));
            let path = folder.join(temporary_name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    let temporary = Self {
                        path,
                        renamed: false,
                    };
                    return Ok((temporary, file));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("no free temporary file name")))
    }

    /// Renames the (closed) file over `target`.
    fn rename_to(mut self, target: &Path) -> io::Result<()> {
        fs::rename(&self.path, target)?;
        self.renamed = true;
        Ok(())
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.renamed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
