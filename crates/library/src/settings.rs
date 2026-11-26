//! Reading and writing the mixer's settings files: a song's `mix.json` and
//! the configuration folder's bus settings and routing, with the rules
//! setlists follow.
//!
//! A file is read only when it resolves inside its folder, symlinks
//! included, is a regular file (a FIFO is refused without being opened) and
//! holds at most [`MAX_SETTINGS_FILE_BYTES`]. A missing file means the
//! defaults. A file that cannot be read or parsed is reported, the defaults
//! are used instead, and the file is left as it is until the user changes a
//! setting, when the new one replaces it. A file with a newer `formatVersion`
//! is never written over, nor is a file whose version cannot be checked
//! because it cannot be read. Every write goes to a temporary file beside the
//! target, which is flushed to disk and then renamed over it, so a file is
//! always either complete or untouched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use domain::MixerFileError;
use thiserror::Error;

use crate::{ConfinedReadError, read_confined, write_file_atomically};

/// The most bytes a settings file may hold.
pub const MAX_SETTINGS_FILE_BYTES: u64 = 1 << 20;

/// Why a settings file (`mix.json`, the bus settings or the routing) could
/// not be read or written. Nothing on disk is changed when one is returned,
/// except that a save whose write fails may leave the configuration folder it
/// created for that write.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// The song folder or the configuration folder cannot be opened or
    /// created.
    #[error("cannot open folder {}: {source}", path.display())]
    Folder {
        /// The folder.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The file resolves, through symlinks or otherwise, outside its folder;
    /// it was not read.
    #[error("{} resolves outside its folder {}", path.display(), folder.display())]
    Outside {
        /// The file.
        path: PathBuf,
        /// The folder it must stay in.
        folder: PathBuf,
    },
    /// The file is a folder, FIFO, device or anything but a regular file; it
    /// was not opened.
    #[error("{} is not a regular file", path.display())]
    NotAFile {
        /// The file.
        path: PathBuf,
    },
    /// The file is larger than [`MAX_SETTINGS_FILE_BYTES`].
    #[error("{} is larger than {max_bytes} bytes, the most a settings file may hold", path.display())]
    TooLarge {
        /// The file.
        path: PathBuf,
        /// [`MAX_SETTINGS_FILE_BYTES`].
        max_bytes: u64,
    },
    /// The file is not UTF-8 text, so it is not JSON any version of this app
    /// wrote; a save replaces it.
    #[error("{} is not UTF-8 text, so it is not a settings file", path.display())]
    NotText {
        /// The file.
        path: PathBuf,
    },
    /// The file cannot be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The file is not valid JSON of the v1 shape or has a `formatVersion`
    /// this version cannot read, or the settings cannot be written as JSON.
    #[error("{}: {source}", path.display())]
    Format {
        /// The file.
        path: PathBuf,
        /// The format error.
        source: MixerFileError,
    },
    /// The file on disk has a newer `formatVersion`, so it was not written
    /// over.
    #[error(
        "{} has formatVersion {found}, newer than this version writes ({supported}); it was not written over",
        path.display()
    )]
    NewerVersion {
        /// The file.
        path: PathBuf,
        /// The version in the file.
        found: u32,
        /// The version this version writes.
        supported: u32,
    },
    /// A file is where the settings go, but it could not be read to check its
    /// `formatVersion` (no permission, too large, or another read error), so
    /// it was not written over.
    #[error(
        "{} was not written over, because its formatVersion could not be checked: {source}",
        path.display()
    )]
    UncheckedVersion {
        /// The file.
        path: PathBuf,
        /// Why it could not be read.
        source: Box<SettingsError>,
    },
    /// The file could not be written. An existing file is left as it was.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
}

/// A settings file as read: the value to use, and why the file could not be
/// used when it could not.
pub(crate) struct Read<T> {
    /// The file's settings, or `T`'s defaults when there is no usable file.
    pub(crate) value: T,
    /// Why the file was not used; `None` when it was, or when there is none.
    pub(crate) error: Option<SettingsError>,
}

/// Reads `file` inside `folder` (canonical) with `parse`: the defaults when
/// there is no file, and the defaults with the reason when it cannot be read
/// or parsed.
pub(crate) fn read_settings<T: Default>(
    folder: &Path,
    file: &str,
    parse: impl FnOnce(&str) -> Result<T, MixerFileError>,
) -> Read<T> {
    let failed = |error| Read {
        value: T::default(),
        error: Some(error),
    };
    let path = folder.join(file);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Read {
                value: T::default(),
                error: None,
            };
        }
        Err(source) => return failed(SettingsError::Read { path, source }),
    }
    let text = match read_confined(folder, file, MAX_SETTINGS_FILE_BYTES) {
        Ok(text) => text,
        Err(error) => return failed(read_error(folder, path, error)),
    };
    match parse(&text) {
        Ok(value) => Read { value, error: None },
        Err(source) => failed(SettingsError::Format { path, source }),
    }
}

fn read_error(folder: &Path, path: PathBuf, error: ConfinedReadError) -> SettingsError {
    match error {
        ConfinedReadError::Outside => SettingsError::Outside {
            path,
            folder: folder.to_owned(),
        },
        ConfinedReadError::NotAFile => SettingsError::NotAFile { path },
        ConfinedReadError::TooLarge => SettingsError::TooLarge {
            path,
            max_bytes: MAX_SETTINGS_FILE_BYTES,
        },
        ConfinedReadError::NotUtf8 => SettingsError::NotText { path },
        ConfinedReadError::Io(source) => SettingsError::Read { path, source },
    }
}

/// Writes `json` as `file` inside `folder` (canonical), atomically, unless the
/// file there now has a newer `formatVersion` ([`SettingsError::NewerVersion`]),
/// which `parse` reads to find out, or cannot be read to check it
/// ([`SettingsError::UncheckedVersion`]). A file that reads but does not
/// parse, or has an older version, is replaced.
pub(crate) fn write_settings<T>(
    folder: &Path,
    file: &str,
    json: &str,
    parse: impl FnOnce(&str) -> Result<T, MixerFileError>,
) -> Result<(), SettingsError> {
    let path = folder.join(file);
    if let Some(text) = current_text(folder, file)?
        && let Err(MixerFileError::UnsupportedFormatVersion {
            found, supported, ..
        }) = parse(&text)
        && found > supported
    {
        return Err(SettingsError::NewerVersion {
            path,
            found,
            supported,
        });
    }
    write_file_atomically(&path, json).map_err(|source| SettingsError::Write { path, source })
}

/// The text now at `file` inside `folder` (canonical), for the version check
/// before a write: `None` when nothing there can hold settings this app wrote
/// (no entry, a dangling link, a link out of the folder, a FIFO, a folder, or
/// a file that is not UTF-8), which the write replaces without losing any (a
/// folder then fails to be replaced). A file that is there but cannot be read
/// (no permission, too large, any other read error) is
/// [`SettingsError::UncheckedVersion`].
fn current_text(folder: &Path, file: &str) -> Result<Option<String>, SettingsError> {
    let path = folder.join(file);
    let unchecked = |source| SettingsError::UncheckedVersion {
        path: path.clone(),
        source: Box::new(source),
    };
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(unchecked(SettingsError::Read {
                path: path.clone(),
                source,
            }));
        }
    }
    match read_confined(folder, file, MAX_SETTINGS_FILE_BYTES) {
        Ok(text) => Ok(Some(text)),
        // JSON is UTF-8, so a file that is not was never written by any
        // version of this app: like a malformed file, it is replaced.
        Err(
            ConfinedReadError::Outside | ConfinedReadError::NotAFile | ConfinedReadError::NotUtf8,
        ) => Ok(None),
        Err(ConfinedReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(unchecked(read_error(folder, path.clone(), error))),
    }
}

/// `to_json`'s result for `file` inside `folder`, as a [`SettingsError`].
pub(crate) fn to_json(
    folder: &Path,
    file: &str,
    json: Result<String, MixerFileError>,
) -> Result<String, SettingsError> {
    json.map_err(|source| SettingsError::Format {
        path: folder.join(file),
        source,
    })
}
