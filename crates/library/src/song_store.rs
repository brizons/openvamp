//! Saving an edited song (S4): [`Library::save_song`] rewrites the
//! `song.json` of a song folder already in the library, atomically, after
//! checking the edited package against the song's stem files as
//! [`crate::load_song`] checks it, and keeps the fields this version does
//! not know. The stems and the [`crate::WAVEFORMS_FOLDER`] are never
//! touched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use domain::{ManifestError, SongPackage};
use serde_json::Value;
use thiserror::Error;

use crate::songs::entry;
use crate::{
    ConfinedReadError, Library, LoadError, MANIFEST_FILE, SongEntry, SongProblem, SongUnavailable,
    check_package, read_confined, write_file_atomically,
};

/// Most bytes [`Library::save_song`] reads from an existing `song.json`:
/// far more than any song's sections and arrangements need.
const MAX_MANIFEST_BYTES: u64 = 64 << 20;

/// Why [`Library::save_song`] did not save a song. Nothing on disk is
/// changed when one is returned.
#[derive(Debug, Error)]
pub enum SaveSongError {
    /// The song path is empty, absolute, or has a `..` component, as this
    /// platform reads paths; nothing on disk was touched.
    #[error("song \"{song}\" is not a relative path inside the library folder")]
    InvalidPath {
        /// The song path.
        song: String,
    },
    /// The song folder resolves, through symlinks or otherwise, outside the
    /// library folder.
    #[error("song \"{song}\" resolves outside the library folder")]
    OutsideLibrary {
        /// The song path.
        song: String,
    },
    /// The library folder or the song folder is missing or cannot be opened.
    #[error("cannot open song folder {}: {source}", path.display())]
    SongFolder {
        /// The folder.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The song's `song.json` is missing or cannot be read: not a regular
    /// file inside the song folder, too large, not UTF-8, or an I/O error.
    #[error("cannot read {}: {source}", path.display())]
    ReadManifest {
        /// Path of the manifest.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// The song's `song.json` is not a manifest this version can rewrite:
    /// malformed, or of a newer `formatVersion`. It was not written over.
    #[error("{} was not written over: {source}", path.display())]
    Manifest {
        /// Path of the manifest.
        path: PathBuf,
        /// Why it cannot be read.
        source: ManifestError,
    },
    /// The edited package breaks one or more rules against the song's stem
    /// files; every problem is listed, as [`crate::load_song`] lists them.
    #[error("the edited song has {} problem(s); song.json was not written", .0.len())]
    Invalid(Vec<SongProblem>),
    /// `song.json` could not be written: it or its folder is read-only, or
    /// an I/O error. The file is left as it was.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// Path of the manifest.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
}

impl Library {
    /// Saves `package` as the `song.json` of the song at `song`, a path
    /// relative to the library folder as [`SongEntry::song`] gives it, and
    /// returns the song as [`Library::list_songs`] would list it.
    ///
    /// The song folder must resolve inside the library folder, symlinks
    /// included, and hold a `song.json` that is a regular file inside it and
    /// a manifest this version reads; a `song.json` of a newer
    /// `formatVersion` is not written over. `package` is checked against the
    /// song's stem files as [`crate::load_song`] checks a song, and refused
    /// with every problem ([`SaveSongError::Invalid`]). The file is then
    /// replaced atomically; a read-only `song.json` is not replaced. Nothing
    /// else in the song folder is touched.
    ///
    /// Fields this version does not know stay: other tools, and newer
    /// versions of this one, may store them. The fields the package owns are
    /// replaced and every other field is written back as it was, at the top
    /// level and in every stem, section, arrangement and map point the edit
    /// keeps (matched by `id`, map points by `at`). A field the edit clears,
    /// such as a removed artist, goes.
    pub fn save_song(&self, song: &str, package: &SongPackage) -> Result<SongEntry, SaveSongError> {
        let folder = self.song_folder(song).map_err(|error| match error {
            SongUnavailable::InvalidPath { song } => SaveSongError::InvalidPath { song },
            SongUnavailable::OutsideLibrary { song } => SaveSongError::OutsideLibrary { song },
            SongUnavailable::Load {
                source: LoadError::SongFolder { path, source },
                ..
            } => SaveSongError::SongFolder { path, source },
            // `song_folder` fails to load nothing else.
            SongUnavailable::Load { song, source } => SaveSongError::SongFolder {
                path: self.root().join(song),
                source: io::Error::other(source.to_string()),
            },
        })?;
        let path = folder.join(MANIFEST_FILE);
        let stored = read_stored(&folder, &path)?;
        let (validated, _stems) =
            check_package(&folder, package.clone()).map_err(SaveSongError::Invalid)?;
        let json =
            keep_unknown_fields(&stored, package).map_err(|source| SaveSongError::Manifest {
                path: path.clone(),
                source,
            })?;
        let read_only = fs::metadata(&path).is_ok_and(|metadata| metadata.permissions().readonly());
        if read_only {
            return Err(SaveSongError::Write {
                path,
                source: io::Error::new(io::ErrorKind::PermissionDenied, "the file is read-only"),
            });
        }
        write_file_atomically(&path, json)
            .map_err(|source| SaveSongError::Write { path, source })?;
        Ok(entry(song, &validated.song))
    }
}

/// The `song.json` at `path` inside `folder` (canonical), as text, when it
/// is a manifest this version reads.
fn read_stored(folder: &Path, path: &Path) -> Result<String, SaveSongError> {
    let unreadable = |source| SaveSongError::ReadManifest {
        path: path.to_owned(),
        source,
    };
    let text = read_confined(folder, MANIFEST_FILE, MAX_MANIFEST_BYTES).map_err(|error| {
        unreadable(match error {
            ConfinedReadError::Io(error) => error,
            ConfinedReadError::Outside => io::Error::new(
                io::ErrorKind::InvalidInput,
                "it resolves outside the song folder",
            ),
            ConfinedReadError::NotAFile => {
                io::Error::new(io::ErrorKind::InvalidInput, "it is not a regular file")
            }
            ConfinedReadError::TooLarge => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("it is larger than {MAX_MANIFEST_BYTES} bytes"),
            ),
            ConfinedReadError::NotUtf8 => {
                io::Error::new(io::ErrorKind::InvalidData, "it is not UTF-8 text")
            }
        })
    })?;
    SongPackage::from_json(&text).map_err(|source| SaveSongError::Manifest {
        path: path.to_owned(),
        source,
    })?;
    Ok(text)
}

/// `package` as `song.json` text, with every field of the `stored` text
/// (a manifest this version reads) that the package does not own. Without
/// such fields it is exactly [`SongPackage::to_json`].
fn keep_unknown_fields(stored: &str, package: &SongPackage) -> Result<String, ManifestError> {
    let new = serde_json::to_value(package).map_err(ManifestError::Json)?;
    let old: Value = serde_json::from_str(stored).map_err(ManifestError::Json)?;
    // What this version reads of the stored file and would write back:
    // every field it owns that the file holds.
    let known =
        serde_json::to_value(SongPackage::from_json(stored)?).map_err(ManifestError::Json)?;
    let merged = merge(old, &known, new.clone());
    if merged == new {
        package.to_json()
    } else {
        serde_json::to_string_pretty(&merged).map_err(ManifestError::Json)
    }
}

/// `new` with the fields of `old` it lacks that are not in `known` (`old`
/// as this version reads it), at every level. Array elements are matched by
/// `id`, or by `at` for map points; `known` is index for index with `old`,
/// since reading keeps every element in order.
fn merge(old: Value, known: &Value, new: Value) -> Value {
    match (old, known, new) {
        (Value::Object(old), Value::Object(known), Value::Object(mut new)) => {
            for (key, old_value) in old {
                if let Some(slot) = new.get_mut(&key) {
                    if let Some(known_value) = known.get(&key) {
                        let new_value = std::mem::take(slot);
                        *slot = merge(old_value, known_value, new_value);
                    }
                } else if !known.contains_key(&key) {
                    new.insert(key, old_value);
                }
            }
            Value::Object(new)
        }
        (Value::Array(old), Value::Array(known), Value::Array(mut new)) => {
            let mut old: Vec<Option<Value>> = old.into_iter().map(Some).collect();
            for new_element in &mut new {
                let Some(key) = element_key(new_element).map(|(name, value)| (name, value.clone()))
                else {
                    continue;
                };
                let matched = old.iter().position(|element| {
                    element
                        .as_ref()
                        .and_then(element_key)
                        .is_some_and(|(name, value)| (name, value) == (key.0, &key.1))
                });
                if let Some(index) = matched
                    && let Some(known_element) = known.get(index)
                    && let Some(old_element) = old[index].take()
                {
                    let new_value = std::mem::take(new_element);
                    *new_element = merge(old_element, known_element, new_value);
                }
            }
            Value::Array(new)
        }
        (_, _, new) => new,
    }
}

/// The field that names an array element: its `id`, or its `at` for a map
/// point.
fn element_key(element: &Value) -> Option<(&'static str, &Value)> {
    let object = element.as_object()?;
    ["id", "at"]
        .into_iter()
        .find_map(|name| object.get(name).map(|value| (name, value)))
}
