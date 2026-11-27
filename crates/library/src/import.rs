//! Song import (F1): a folder or ZIP of stems becomes a song package in the
//! library folder.
//!
//! [`probe_import`] reads a source and returns an [`ImportDraft`]: one
//! [`ProbedStem`] per audio file, with the facts read from it, what the
//! import will do with it, and the [`SongDraft`] the user edits (title, key,
//! tempo, time signature, changes at bars, and parts). [`Library::save_import`]
//! then writes the song.
//!
//! # Sources
//!
//! A source is a folder or a `.zip` file. Every audio file in it is taken, in
//! its subfolders too, sorted by path: a file whose extension is `.wav`,
//! `.wave`, `.flac`, `.mp3` or `.m4a` (in any case), whose content the
//! import can then decode. Hidden files and folders (starting with `.`), the
//! `__MACOSX` folder of ZIP files made on macOS, symlinked folders and files
//! with other extensions are left out; a source without any audio file is
//! refused ([`ImportError::NoAudio`]).
//!
//! A ZIP entry's name is only shown and read for its extension; nothing is
//! ever written under it. To read an entry, the import extracts it to a
//! temporary file with a name it makes up, in a hidden folder (the system's
//! temporary folder while probing, the hidden folder of the song being built
//! while saving), and removes it afterwards. Stored and deflated entries
//! are read; any other compression or an encrypted entry is refused.
//!
//! # What is copied and what is transcoded
//!
//! - A WAV or FLAC file the player reads as it is (through the same reader
//!   [`crate::load_song`] uses: 16-, 24- or 32-bit integer or 32-bit float
//!   WAV, or FLAC, mono or stereo) is copied byte for byte
//!   ([`StemAction::Copy`]), as `stems/<id>.wav` or `stems/<id>.flac` by its
//!   content, whatever its extension says.
//! - MP3, AAC in M4A, and any WAV or FLAC the player cannot read as it is
//!   (an 8-bit or 64-bit float WAV, a FLAC that starts with an ID3 tag) are
//!   decoded and written as 24-bit FLAC, `stems/<id>.flac`
//!   ([`StemAction::Transcode`]), so every stem seeks exactly. Samples are
//!   rounded to 24 bits and clamped, without dither: a source of 24 bits or
//!   fewer converts exactly. No FLAC frame is larger than its samples stored
//!   verbatim, so loud noise takes at most about its raw 24-bit size.
//!   Encoder delay and padding are trimmed when the file records them, so
//!   every stem starts on the same sample: the LAME or Xing header of an
//!   MP3; the edit list of an M4A, or the `iTunSMPB` tag Apple's encoder
//!   writes instead. An MP3 without such a header keeps its encoder's and
//!   decoder's delay (1 105 frames for LAME), and an M4A that records
//!   neither keeps its priming.
//! - The length the probe reads is exact, except for an MP3 without a Xing
//!   or LAME header and an M4A that records no delay, whose length is
//!   estimated from their packets ([`ProbedStem::estimated_length`]); such
//!   a stem is saved at the length its decoder gives, which can be a packet
//!   shorter.
//! - A file with more than two channels, an empty one, one nothing decodes,
//!   or a WAV or FLAC whose audio stops before the length its header
//!   declares, is refused ([`RefusedFile`]), with its name and the reason; a
//!   draft with a refused file is not saved.
//!
//! # Saving
//!
//! [`Library::save_import`] builds the song in a hidden folder inside the
//! library folder (named `.import-…`, so [`Library::list_songs`] never shows
//! it): it copies or transcodes every stem, then reads every stem written
//! to its end with the player's own reader, a block at a time, checking its
//! sample rate and length against what the probe read; it writes
//! `song.json` and opens the whole song as [`crate::load_song`] does. Only
//! then is the folder renamed into place, at once, as `<Title>.song` in the
//! library folder. On any failure, or when cancelled, the hidden folder is
//! removed and nothing else changes; an error about a stem names it
//! ([`ImportError::Stem`]).
//!
//! The folder name is the title made safe on Windows, macOS and Linux:
//! `/`, `\` and `:` become `-`; `* ? " < > |` and control characters are
//! left out; leading and trailing dots and spaces are trimmed; a name Windows
//! reserves for a device (`CON`, `NUL`, `COM1`…) gets a `_`; it is cut to at
//! most 200 bytes; and an empty result is `Untitled`. When a song folder of
//! that name exists, " 2", " 3" and so on are added. As for setlists, the
//! library assumes one writer: another program could take the name between
//! the check and the rename.

use std::fmt;
use std::path::{Path, PathBuf};

use domain::{DraftError, SongDraft};
use thiserror::Error;

#[cfg(doc)]
use crate::Library;
use crate::SetlistError;

mod audio;
mod mp4;
mod save;
mod source;

pub use save::{SONG_FOLDER_EXTENSION, song_folder_name};

/// The audio format of a source file, from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceFormat {
    /// `.wav` or `.wave`.
    Wav,
    /// `.flac`.
    Flac,
    /// `.mp3`.
    Mp3,
    /// `.m4a`: AAC in an MPEG-4 file.
    M4a,
}

impl SourceFormat {
    /// The format a file name's extension says, ignoring case; `None` for
    /// anything but audio.
    pub fn from_name(name: &str) -> Option<Self> {
        let (_, extension) = name.rsplit_once('.')?;
        match extension.to_ascii_lowercase().as_str() {
            "wav" | "wave" => Some(Self::Wav),
            "flac" => Some(Self::Flac),
            "mp3" => Some(Self::Mp3),
            "m4a" => Some(Self::M4a),
            _ => None,
        }
    }
}

impl fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
            Self::Mp3 => "MP3",
            Self::M4a => "M4A",
        })
    }
}

/// What the import does with a stem's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StemAction {
    /// The file is copied byte for byte.
    Copy,
    /// The file is decoded and written as 24-bit FLAC.
    Transcode {
        /// Why: the file is lossy, or why the player cannot read it as it
        /// is.
        reason: String,
    },
}

/// An audio file of the source the import will save as a stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedStem {
    /// Its path inside the source, with `/` between folders.
    pub name: String,
    /// The format its extension says.
    pub format: SourceFormat,
    /// Channels: 1 or 2.
    pub channels: u16,
    /// What the import does with it.
    pub action: StemAction,
    /// Whether the length the probe read is only an estimate: an MP3
    /// without a Xing or LAME header, or an M4A that records neither an edit
    /// list nor an `iTunSMPB` tag. Such a stem is saved at the length its
    /// decoder gives, which can be a packet shorter.
    pub estimated_length: bool,
    /// Where to read it.
    pub(crate) location: Location,
    /// Frames of decoded audio to drop before the stem starts: the encoder
    /// delay a file records that its decoder does not drop itself.
    pub(crate) skip_frames: u64,
}

/// Where a source file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Location {
    /// A file at this path relative to the source folder, `/` between
    /// folders.
    File(String),
    /// The ZIP entry with this index.
    Entry(usize),
}

/// An audio file of the source the import will not save.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("\"{name}\": {reason}")]
pub struct RefusedFile {
    /// Its path inside the source, with `/` between folders.
    pub name: String,
    /// Why, such as "it has 6 channels; stems must be mono or stereo".
    pub reason: String,
}

/// A source read by [`probe_import`], ready to be edited and saved.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportDraft {
    /// The song the user edits: title, key, tempo, time signature, changes
    /// and parts. Its stems are [`Self::stems`], in the same order.
    pub song: SongDraft,
    stems: Vec<ProbedStem>,
    refused: Vec<RefusedFile>,
    source: Source,
}

/// The folder or ZIP file an import reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    /// A folder, canonical.
    Folder(PathBuf),
    /// A ZIP file, canonical.
    Zip(PathBuf),
}

impl Source {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Folder(path) | Self::Zip(path) => path,
        }
    }
}

impl ImportDraft {
    /// The audio files the import will save, in the order of
    /// [`SongDraft::stems`].
    pub fn stems(&self) -> &[ProbedStem] {
        &self.stems
    }

    /// The audio files the import cannot save; while there is one, the
    /// draft is not saved.
    pub fn refused(&self) -> &[RefusedFile] {
        &self.refused
    }

    /// The folder or ZIP file the draft was read from, canonical.
    pub fn source(&self) -> &Path {
        self.source.path()
    }
}

/// How far [`Library::save_import`] has got, in frames of audio written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportProgress {
    /// The stem this update is about, an index into
    /// [`ImportDraft::stems`].
    pub stem: usize,
    /// Frames of that stem written so far.
    pub stem_done: u64,
    /// Frames of that stem in all.
    pub stem_frames: u64,
    /// Frames written so far, over all stems.
    pub done: u64,
    /// Frames to write, over all stems.
    pub total: u64,
}

impl ImportProgress {
    /// The share done, from 0 to 1.
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.done as f64 / self.total as f64
        }
    }
}

/// Why a source cannot be read or a draft cannot be saved.
#[derive(Debug, Error)]
pub enum ImportError {
    /// The source cannot be opened or read.
    #[error("cannot read {}: {source}", path.display())]
    Source {
        /// The source.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The source is neither a folder nor a `.zip` file.
    #[error("{} is neither a folder nor a .zip file", path.display())]
    NotFolderOrZip {
        /// The source.
        path: PathBuf,
    },
    /// The `.zip` file is not a ZIP archive the import can read.
    #[error("{} is not a readable ZIP file: {reason}", path.display())]
    Zip {
        /// The source.
        path: PathBuf,
        /// What the ZIP reader said.
        reason: String,
    },
    /// The source holds no WAV, FLAC, MP3 or M4A file.
    #[error("{} holds no WAV, FLAC, MP3 or M4A file", path.display())]
    NoAudio {
        /// The source.
        path: PathBuf,
    },
    /// The source holds more audio files than a song can import.
    #[error("{} holds more than {max} audio files", path.display())]
    TooManyFiles {
        /// The source.
        path: PathBuf,
        /// The most a song imports.
        max: usize,
    },
    /// Files of the draft the import cannot save.
    #[error("{} file(s) cannot be imported: {}", .0.len(), list(.0))]
    Refused(Vec<RefusedFile>),
    /// The draft breaks a rule; every problem is listed.
    #[error("the song cannot be saved: {}", list(.0))]
    Draft(Vec<DraftError>),
    /// The library folder cannot be used.
    #[error(transparent)]
    Library(#[from] SetlistError),
    /// A file of the song could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The file or folder.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// A stem could not be read, decoded or checked while saving.
    #[error("\"{name}\": {reason}")]
    Stem {
        /// The stem's path inside the source.
        name: String,
        /// What went wrong.
        reason: String,
    },
    /// The draft's stems are not the files its source was read with: its
    /// [`SongDraft`] was replaced by another one.
    #[error("the draft's stems are not the files its source was read with")]
    StemsChanged,
    /// The save was cancelled; nothing was left behind.
    #[error("the import was cancelled")]
    Cancelled,
}

fn list<T: fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Most audio files one import takes.
pub const MAX_IMPORT_FILES: usize = 256;

/// Reads the folder or `.zip` file at `source` as the [module docs](self)
/// describe, without decoding any audio but the first packet of the files
/// it transcodes, and returns the draft to edit and save.
///
/// The draft's title is the folder's name, or the ZIP file's name without
/// its extension; it starts at 120 BPM in 4/4 with no key, and each stem's
/// part comes from its file name. Files it cannot import are in
/// [`ImportDraft::refused`].
pub fn probe_import(source: &Path) -> Result<ImportDraft, ImportError> {
    source::probe(source)
}
