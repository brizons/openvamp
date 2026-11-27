//! The songs of the library folder (F10.2): every song folder in it, read
//! without decoding any audio, so a shell can offer them for a setlist.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use domain::Song;

use crate::{
    Library, LoadError, MANIFEST_FILE, OpenFileError, SETLISTS_FOLDER, SetlistError,
    SongUnavailable, open_package_json, open_regular_file, resolve_inside,
};

/// A song folder that [`Library::list_songs`] could read, with what its
/// `song.json` holds.
#[derive(Debug, Clone, PartialEq)]
pub struct SongEntry {
    /// Path of the song folder relative to the library folder, with `/`
    /// between folders, such as `Hymns/Example Song.song`: what a setlist
    /// item stores ([`domain::SongItem::song`]).
    pub song: String,
    /// Song title.
    pub title: String,
    /// Artist, when the package names one.
    pub artist: Option<String>,
    /// Musical key, when the package names one.
    pub key: Option<String>,
    /// Tempo at the start of the song, in beats per minute.
    pub bpm: f64,
    /// Frames per second of the song.
    pub sample_rate: u32,
    /// Number of stems.
    pub stems: usize,
    /// Song length in frames: the length of the longest stem.
    pub length_frames: u64,
    /// Arrangements in package order.
    pub arrangements: Vec<ArrangementEntry>,
}

/// One arrangement of a [`SongEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrangementEntry {
    /// Arrangement id, what a setlist item stores
    /// ([`domain::SongItem::arrangement`]).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Its positions in play order; a setlist item's start and end points
    /// index into them.
    pub positions: Vec<PositionEntry>,
}

impl ArrangementEntry {
    /// Frames the whole arrangement plays.
    pub fn length_frames(&self) -> u64 {
        self.positions.iter().map(|p| p.length_frames).sum()
    }
}

/// One position of an [`ArrangementEntry`]: a section in play order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionEntry {
    /// Section id, as in the song package.
    pub section_id: String,
    /// The section type and its number, such as "Verse 2" or "Pre-chorus",
    /// as [`domain::SectionType::label`] words it, like the live player's
    /// song description.
    pub label: String,
    /// Frames in the section.
    pub length_frames: u64,
}

/// A song folder that [`Library::list_songs`] could not read, or a folder
/// it could not look into.
#[derive(Debug)]
pub struct UnreadableSong {
    /// Path of the folder relative to the library folder, with `/` between
    /// folders.
    pub song: String,
    /// Why it cannot be read.
    pub error: SongUnavailable,
}

/// What [`Library::list_songs`] found.
#[derive(Debug, Default)]
pub struct SongListing {
    /// Readable songs, sorted by title ignoring case, then by title, then by
    /// path.
    pub songs: Vec<SongEntry>,
    /// Song folders that cannot be read, and folders that could not be
    /// looked into, sorted by path; one broken song hides no other.
    pub unreadable: Vec<UnreadableSong>,
}

impl Library {
    /// Lists the song folders of the library: every folder, at any depth,
    /// that holds a `song.json`. A song folder is not looked into further.
    ///
    /// Skipped: hidden entries (starting with `.`), names that are not
    /// UTF-8 (a setlist could not name them), the [`SETLISTS_FOLDER`], and
    /// symlinked folders without a `song.json` (never followed, so a link
    /// cannot lead the listing out of the library or round in a circle).
    ///
    /// Each song is read as [`Library::setlist_songs`] reads it: its folder
    /// must resolve inside the library folder, symlinks included; its
    /// `song.json` too, and it must be a regular file (a FIFO is refused
    /// without being opened); then the package is checked with its stem
    /// headers, without decoding any audio. A song that fails is listed in
    /// [`SongListing::unreadable`] with its reason, as is a folder that
    /// cannot be read.
    pub fn list_songs(&self) -> Result<SongListing, SetlistError> {
        let root = self.canonical_root()?;
        let mut listing = SongListing::default();
        self.list_folder(&root, &root, "", &mut listing)
            .map_err(|source| SetlistError::Folder {
                path: root.clone(),
                source,
            })?;
        listing.songs.sort_by(by_title);
        listing.unreadable.sort_by(|a, b| a.song.cmp(&b.song));
        Ok(listing)
    }

    /// Lists the songs in `folder`, at `prefix` inside the library folder
    /// `root` (canonical; `prefix` is empty for the library folder itself),
    /// and in the folders inside it. An error is about `folder` itself; one
    /// about a folder inside it is listed as unreadable.
    fn list_folder(
        &self,
        root: &Path,
        folder: &Path,
        prefix: &str,
        listing: &mut SongListing,
    ) -> io::Result<()> {
        let entries = fs::read_dir(folder)?.map(|found| {
            found.map(|entry| Found {
                name: entry.file_name(),
                path: entry.path(),
                is_dir: entry.file_type().is_ok_and(|kind| kind.is_dir()),
            })
        });
        self.list_entries(root, folder, prefix, entries, listing);
        Ok(())
    }

    /// Lists the songs among `entries`, the entries of `folder` at `prefix`
    /// (see [`Self::list_folder`]). An entry that cannot be read is listed
    /// as unreadable under the folder's path, and the others are still
    /// listed; after [`MAX_ENTRY_ERRORS`] of them the folder is left.
    fn list_entries(
        &self,
        root: &Path,
        folder: &Path,
        prefix: &str,
        entries: impl Iterator<Item = io::Result<Found>>,
        listing: &mut SongListing,
    ) {
        let mut errors = 0;
        for found in entries {
            let entry = match found {
                Ok(entry) => entry,
                Err(source) => {
                    let song = if prefix.is_empty() { "." } else { prefix }.to_owned();
                    listing.unreadable.push(UnreadableSong {
                        song: song.clone(),
                        error: SongUnavailable::Load {
                            song,
                            source: LoadError::SongFolder {
                                path: folder.to_owned(),
                                source,
                            },
                        },
                    });
                    errors += 1;
                    if errors == MAX_ENTRY_ERRORS {
                        return;
                    }
                    continue;
                }
            };
            let Ok(name) = entry.name.into_string() else {
                continue;
            };
            if name.starts_with('.') || (prefix.is_empty() && name == SETLISTS_FOLDER) {
                continue;
            }
            let song = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path;
            if fs::symlink_metadata(path.join(MANIFEST_FILE)).is_ok() {
                match self.read_song(root, &song) {
                    Ok(entry) => listing.songs.push(entry),
                    Err(error) => listing.unreadable.push(UnreadableSong { song, error }),
                }
            } else if entry.is_dir
                && let Err(source) = self.list_folder(root, &path, &song, listing)
            {
                listing.unreadable.push(UnreadableSong {
                    song: song.clone(),
                    error: SongUnavailable::Load {
                        song,
                        source: LoadError::SongFolder { path, source },
                    },
                });
            }
        }
    }

    /// Reads the song folder at `song` inside the library folder `root`
    /// (canonical), as [`Library::setlist_songs`] reads it, after checking
    /// that its `song.json` resolves inside the library and is a regular
    /// file.
    fn read_song(&self, root: &Path, song: &str) -> Result<SongEntry, SongUnavailable> {
        let folder = self.song_folder(song)?;
        let load = |source| SongUnavailable::Load {
            song: song.to_owned(),
            source,
        };
        let manifest = match resolve_inside(root, Path::new(song).join(MANIFEST_FILE)) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => {
                return Err(SongUnavailable::OutsideLibrary {
                    song: song.to_owned(),
                });
            }
            Err(source) => {
                return Err(load(LoadError::ReadManifest {
                    path: folder.join(MANIFEST_FILE),
                    source,
                }));
            }
        };
        // The file checked is the file read: parsed from the handle opened,
        // never through the path again.
        let (mut file, _) = open_regular_file(&manifest).map_err(|error| {
            load(match error {
                OpenFileError::Io(source) => LoadError::ReadManifest {
                    path: manifest.clone(),
                    source,
                },
                OpenFileError::NotRegular => LoadError::ManifestNotAFile {
                    path: manifest.clone(),
                },
                OpenFileError::Changed => LoadError::ManifestChanged {
                    path: manifest.clone(),
                },
            })
        })?;
        let mut json = String::new();
        file.read_to_string(&mut json).map_err(|source| {
            load(LoadError::ReadManifest {
                path: manifest.clone(),
                source,
            })
        })?;
        let (validated, _stems) = open_package_json(&folder, &json).map_err(load)?;
        Ok(entry(song, &validated.song))
    }
}

/// Entry errors after which [`Library::list_entries`] leaves a folder, so a
/// folder whose reads keep failing cannot keep the listing going forever.
const MAX_ENTRY_ERRORS: usize = 100;

/// One entry of a folder being listed.
struct Found {
    name: OsString,
    path: PathBuf,
    /// Whether it is a folder itself, not a symlink to one.
    is_dir: bool,
}

/// What the listing shows of `song`, found at `path`.
pub(crate) fn entry(path: &str, song: &Song) -> SongEntry {
    let arrangements = song
        .arrangements()
        .iter()
        .map(|arrangement| ArrangementEntry {
            id: arrangement.id.clone(),
            name: arrangement.name.clone(),
            positions: arrangement
                .sections
                .iter()
                .filter_map(|id| song.section(id))
                .map(|section| PositionEntry {
                    section_id: section.id.clone(),
                    label: section.section_type.label(section.number),
                    length_frames: section.length(),
                })
                .collect(),
        })
        .collect();
    SongEntry {
        song: path.to_owned(),
        title: song.title().to_owned(),
        artist: song.artist().map(str::to_owned),
        key: song.key().map(str::to_owned),
        // A validated tempo map starts at 0, so it has a first point.
        bpm: song.tempo_map().first().map_or(0.0, |point| point.bpm),
        sample_rate: song.sample_rate(),
        stems: song.stems().len(),
        length_frames: song.length(),
        arrangements,
    }
}

/// Sorts entries by title ignoring case, then by title, then by path.
fn by_title(a: &SongEntry, b: &SongEntry) -> Ordering {
    a.title
        .to_lowercase()
        .cmp(&b.title.to_lowercase())
        .then_with(|| a.title.cmp(&b.title))
        .then_with(|| a.song.cmp(&b.song))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_that_cannot_be_read_is_listed_apart_and_the_walk_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::new(dir.path());
        let root = fs::canonicalize(dir.path()).unwrap();
        let entries = [
            Err(io::Error::other("the disk said no")),
            // A folder that is gone by the time it is read: listed apart
            // too, which shows the walk went on past the failed entry.
            Ok(Found {
                name: "Gone".into(),
                path: root.join("Gone"),
                is_dir: true,
            }),
        ];
        let mut listing = SongListing::default();

        library.list_entries(&root, &root, "", entries.into_iter(), &mut listing);

        let unreadable: Vec<&str> = listing.unreadable.iter().map(|u| u.song.as_str()).collect();
        assert_eq!(unreadable, [".", "Gone"]);
        assert!(
            listing.unreadable[0]
                .error
                .to_string()
                .ends_with("the disk said no"),
            "{}",
            listing.unreadable[0].error
        );
    }

    #[test]
    fn a_folder_whose_entries_keep_failing_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::new(dir.path());
        let root = fs::canonicalize(dir.path()).unwrap();
        let mut listing = SongListing::default();

        library.list_entries(
            &root,
            &root,
            "Hymns",
            std::iter::repeat_with(|| Err(io::Error::other("again"))),
            &mut listing,
        );

        assert_eq!(listing.unreadable.len(), MAX_ENTRY_ERRORS);
        assert!(listing.unreadable.iter().all(|u| u.song == "Hymns"));
    }
}
