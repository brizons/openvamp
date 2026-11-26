//! Setlists in the library folder (F10.2): `*.setlist.json` files in its
//! [`SETLISTS_FOLDER`], read and written with the same care as song packages.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use domain::{Setlist, SetlistEditError, SetlistFileError};
use thiserror::Error;

use crate::setlist_order::{apply_order, read_order, update_order, write_order};
use crate::{ConfinedReadError, read_confined, resolve_inside, same_file, write_file_atomically};

/// Name of the folder, inside the library folder, that holds the setlists.
pub const SETLISTS_FOLDER: &str = "Setlists";

/// Ending of every setlist file name.
pub const SETLIST_EXTENSION: &str = ".setlist.json";

/// Name of the file, inside the [`SETLISTS_FOLDER`], that holds the order
/// of the setlist list (see [the setlist order](Library#the-setlist-order)).
pub const SETLIST_ORDER_FILE: &str = "order.json";

/// The only `formatVersion` of the [`SETLIST_ORDER_FILE`] this version
/// reads and writes.
pub const SETLIST_ORDER_FORMAT_VERSION: u32 = 1;

/// Largest setlist file read, in bytes. A setlist of a few hundred items is
/// well under 1 MiB; anything larger is refused before it is read.
pub const MAX_SETLIST_FILE_BYTES: u64 = 1 << 20;

/// Longest file name stem, in bytes, a setlist name maps to.
const MAX_FILE_STEM_BYTES: usize = 200;

/// Names Windows reserves for devices, whatever follows the first dot.
const WINDOWS_RESERVED_NAMES: [&str; 30] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// A library folder: songs in folders at paths relative to it, setlists in
/// its [`SETLISTS_FOLDER`].
///
/// Creating a `Library` touches nothing on disk; every operation resolves
/// the folder again, so it may be created, moved back or renamed in between.
///
/// # Setlist file names
///
/// A setlist's file name comes from its name, so the folder stays readable
/// by a person: `Sunday Morning` is stored as `Sunday Morning.setlist.json`.
/// The name stored inside the file is the one that counts; the file name
/// only has to be safe on Windows, macOS and Linux, since a library folder
/// can move between them:
///
/// - `/ \ : * ? " < > |`, ASCII control characters, `%` itself, and a leading
///   `.` (which would hide the file) are written as `%XX`, the character's
///   ASCII code in hexadecimal, as in a URL: `AC/DC` becomes `AC%2FDC`.
/// - When the part before the first `.`, ignoring case and trailing spaces,
///   is a name Windows reserves for a device (`CON`, `PRN`, `AUX`, `NUL`,
///   `COM0`–`COM9`, `LPT0`–`LPT9`, and `COM`/`LPT` with `¹ ² ³`), its first
///   letter is escaped too: `CON` becomes `%43ON`.
/// - The part before [`SETLIST_EXTENSION`] is cut to at most 200 bytes, at a
///   character boundary and never inside an escape, so the file name and the
///   temporary file written beside it stay under the 255-byte limit of common
///   file systems (NTFS counts UTF-16 units, never more than UTF-8 bytes).
///
/// Decoding the `%XX` escapes gives the name back unless it was cut. Two
/// names can still share a file name: when cut, or on a file system that
/// ignores case or Unicode normalization (as macOS and Windows do by
/// default). Saving, renaming or duplicating never overwrites a file that
/// holds another setlist: it returns [`SetlistError::FileTaken`].
///
/// # The setlist order
///
/// The order of the setlist list (F5.1) is stored in the
/// [`SETLIST_ORDER_FILE`] of the [`SETLISTS_FOLDER`], format v1: the setlist
/// file names in order ([`Library::move_setlist`] writes it). Setlists it
/// does not name follow in name order, and names of files that no longer
/// exist are ignored. An order file that cannot be read or is not a valid
/// order is reported in [`SetlistListing::order_error`] and the list falls
/// back to name order; no setlist is hidden or lost because of it. While
/// an order is stored, a new setlist goes at the end, a renamed one keeps
/// its place, a duplicate goes right after its original, and a removed one
/// leaves it ([`Library::remove_setlist`]); without one,
/// nothing writes an order file but a move. No setlist file changes when
/// the order does.
///
/// # Removing
///
/// [`Library::remove_setlist`] takes a setlist out of the list by moving
/// its file to the system trash (through the `trash` crate), where it can
/// be restored; no setlist file is ever deleted.
///
/// # Writing
///
/// Every write goes to a temporary file in the [`SETLISTS_FOLDER`], which is
/// flushed to disk and then renamed over the setlist file, so a setlist file
/// is always either complete or untouched. Between checking that a file name
/// is free and the rename, another program could create that file; this
/// library assumes one writer per library folder.
#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
    /// What moves a removed setlist's file to the trash.
    trash: TrashFn,
}

/// Two libraries are the same when they are the same folder; the trash
/// they use does not count.
impl PartialEq for Library {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl Eq for Library {}

/// Moves the file at a path to the trash; [`Library::remove_setlist`] calls
/// it. [`move_to_trash`] unless replaced with [`Library::with_trash`].
pub type TrashFn = fn(&Path) -> io::Result<()>;

/// Moves the file at `path` to the system trash (the `trash` crate: the
/// Trash on macOS, the Recycle Bin on Windows, the freedesktop.org trash on
/// Linux), where the person can restore it.
///
/// On macOS it asks `NSFileManager` directly. The crate's default there
/// asks the Finder through AppleScript, which can bring up a permission
/// prompt for automating the Finder while the calling thread (the screen's,
/// in the desktop app) waits for it. `NSFileManager` needs no permission;
/// what it gives up is the Finder's sound and its "Put Back" for the file,
/// which the person can still restore by dragging it out of the Trash.
/// Elsewhere the crate's default is used: a setlist file is a few KiB, so
/// moving it is quick.
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let context = {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut context = trash::TrashContext::new();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context
    };
    #[cfg(not(target_os = "macos"))]
    let context = trash::TrashContext::new();
    context.delete(path).map_err(io::Error::other)
}

/// A setlist file that [`Library::list_setlists`] could read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetlistEntry {
    /// The name stored in the setlist.
    pub name: String,
    /// The file name inside the [`SETLISTS_FOLDER`], such as
    /// `Sunday Morning.setlist.json`: what to pass to
    /// [`Library::load_setlist`], [`Library::rename_setlist`] and
    /// [`Library::duplicate_setlist`].
    pub file: String,
}

/// A setlist file that [`Library::list_setlists`] could not read.
#[derive(Debug)]
pub struct UnreadableSetlist {
    /// The file name inside the [`SETLISTS_FOLDER`].
    pub file: String,
    /// Why it cannot be read.
    pub error: SetlistError,
}

/// What [`Library::list_setlists`] found.
#[derive(Debug, Default)]
pub struct SetlistListing {
    /// Readable setlists in the stored order (see
    /// [the setlist order](Library#the-setlist-order)); those it does not
    /// name follow, sorted by name ignoring case, then by name, then by
    /// file.
    pub setlists: Vec<SetlistEntry>,
    /// Setlist files that cannot be read, sorted by file name; one broken
    /// file hides no other setlist.
    pub unreadable: Vec<UnreadableSetlist>,
    /// Why the [`SETLIST_ORDER_FILE`] could not be used, when it is there
    /// but cannot be read or is not a valid order: the setlists are then
    /// listed by name.
    pub order_error: Option<SetlistError>,
    /// Entries of the [`SETLISTS_FOLDER`] that could not be read at all, so
    /// not even their names are known; every other setlist is listed.
    pub entry_errors: Vec<SetlistError>,
}

/// Why a setlist could not be listed, read or written. Nothing on disk is
/// changed when one is returned, except where a variant says otherwise.
#[derive(Debug, Error)]
pub enum SetlistError {
    /// The library folder or its setlists folder cannot be opened.
    #[error("cannot open folder {}: {source}", path.display())]
    Folder {
        /// The folder.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The setlists folder or a setlist file resolves, through symlinks or
    /// otherwise, outside the library folder or the setlists folder.
    #[error("{} resolves outside the library's {SETLISTS_FOLDER} folder", path.display())]
    OutsideLibrary {
        /// The path as asked for, before symlinks were followed.
        path: PathBuf,
    },
    /// The name given is not a setlist file name.
    #[error(
        "\"{file}\" is not a setlist file name: it must be a file name ending in {SETLIST_EXTENSION}, without folders, not starting with a dot"
    )]
    InvalidFileName {
        /// The rejected file name.
        file: String,
    },
    /// The setlist file is a folder, FIFO, device or anything but a regular
    /// file; it was not opened.
    #[error("{} is not a regular file", path.display())]
    NotAFile {
        /// The setlist file.
        path: PathBuf,
    },
    /// The setlist file is larger than [`MAX_SETLIST_FILE_BYTES`].
    #[error("{} is larger than {max_bytes} bytes, the most a setlist file may hold", path.display())]
    TooLarge {
        /// The setlist file.
        path: PathBuf,
        /// [`MAX_SETLIST_FILE_BYTES`].
        max_bytes: u64,
    },
    /// The setlist file cannot be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The setlist file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The setlist file is not a valid v1 setlist, or the setlist cannot be
    /// written as one.
    #[error("{}: {source}", path.display())]
    Format {
        /// The setlist file.
        path: PathBuf,
        /// The format error.
        source: SetlistFileError,
    },
    /// The setlist file could not be written. An existing file at `path` is
    /// left as it was.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The setlist file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The file the setlist name maps to already holds another setlist, so
    /// it was not overwritten.
    #[error(
        "setlist \"{name}\" would be stored as {file}, which another setlist already uses; choose another name"
    )]
    FileTaken {
        /// The setlist name.
        name: String,
        /// The file name it maps to.
        file: String,
    },
    /// The setlist edit was refused (a blank name, or a duplicate keeping
    /// the original's name).
    #[error(transparent)]
    Edit(#[from] SetlistEditError),
    /// The [`SETLIST_ORDER_FILE`] is not a valid order: malformed JSON, or
    /// not the v1 shape.
    #[error("{} is not a valid setlist order: {reason}", path.display())]
    OrderFormat {
        /// The order file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The [`SETLIST_ORDER_FILE`] declares a `formatVersion` this version
    /// cannot read; it is never written over.
    #[error(
        "{}: unsupported formatVersion {found}; this version reads formatVersion {SETLIST_ORDER_FORMAT_VERSION}",
        path.display()
    )]
    OrderVersion {
        /// The order file.
        path: PathBuf,
        /// The version in the file.
        found: u32,
    },
    /// The setlist was written, but its place in the stored order could
    /// not be: until it is moved, it is listed after the setlists the order
    /// names. Only its place is missing; the save itself succeeded.
    #[error(
        "setlist \"{}\" was saved as {}, but its place in the list could not be stored: {reason}",
        saved.name,
        saved.file
    )]
    OrderNotUpdated {
        /// The setlist as saved.
        saved: SetlistEntry,
        /// Why its place could not be stored.
        reason: String,
    },
    /// No setlist file of the [`SETLISTS_FOLDER`] is listed with exactly
    /// this name; names are compared as the folder lists them, case
    /// included, also where the disk ignores case.
    #[error(
        "no setlist file is listed as \"{file}\" in the {SETLISTS_FOLDER} folder (names are compared exactly, case included)"
    )]
    NotListed {
        /// The name asked for.
        file: String,
    },
    /// The trash refused the setlist file, which stays where it was.
    #[error("cannot move {} to the trash: {source}", path.display())]
    Trash {
        /// The setlist file.
        path: PathBuf,
        /// Why the trash refused it.
        source: io::Error,
    },
    /// The setlist file was moved to the trash, but the stored order could
    /// not be updated: the name it keeps names no file, so it is ignored.
    #[error(
        "setlist \"{}\" was moved to the trash, but the order of the list could not be updated: {reason}",
        removed.name
    )]
    RemovedOrderNotUpdated {
        /// The setlist removed.
        removed: SetlistEntry,
        /// Why the order could not be updated.
        reason: String,
    },
    /// A setlist was asked to move past the end of the list.
    #[error("cannot move a setlist to place {}: the library lists {len} setlists", .to.saturating_add(1))]
    MoveOutOfRange {
        /// The place asked for, counted from 0.
        to: usize,
        /// How many setlists the library lists.
        len: usize,
    },
}

impl Library {
    /// The library at the folder `root`, removing setlists to the system
    /// trash ([`move_to_trash`]).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            trash: move_to_trash,
        }
    }

    /// The same library, moving removed setlists with `trash` instead of to
    /// the system trash: tests use it so they never fill a real trash.
    pub fn with_trash(self, trash: TrashFn) -> Self {
        Self { trash, ..self }
    }

    /// The library folder, as given to [`Library::new`].
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Lists the setlist files in the [`SETLISTS_FOLDER`]: every entry whose
    /// name ends in [`SETLIST_EXTENSION`], except hidden ones (starting with
    /// `.`, such as temporary files and the `._` files macOS leaves on other
    /// file systems) and names that are not UTF-8. Each one is read as
    /// [`Library::load_setlist`] reads it. A library without a setlists
    /// folder has no setlists.
    pub fn list_setlists(&self) -> Result<SetlistListing, SetlistError> {
        let Some(folder) = self.setlists_folder()? else {
            return Ok(SetlistListing::default());
        };
        let mut listing = SetlistListing::default();
        let (files, errors) = read_setlist_names(&folder)?;
        listing.entry_errors = errors
            .into_iter()
            .map(|source| SetlistError::Folder {
                path: folder.clone(),
                source,
            })
            .collect();
        for file in files {
            match read_setlist(&folder, &file) {
                Ok(setlist) => listing.setlists.push(SetlistEntry {
                    name: setlist.name,
                    file,
                }),
                Err(error) => listing.unreadable.push(UnreadableSetlist { file, error }),
            }
        }
        listing.setlists.sort_by(by_name);
        listing.unreadable.sort_by(|a, b| a.file.cmp(&b.file));
        match read_order(&folder) {
            Ok(Some(order)) => {
                listing.setlists = apply_order(std::mem::take(&mut listing.setlists), &order);
            }
            Ok(None) => {}
            Err(error) => listing.order_error = Some(error),
        }
        Ok(listing)
    }

    /// Moves the setlist in `file` to place `to` (counted from 0) of the
    /// list [`Library::list_setlists`] returns, and stores the whole order
    /// in the [`SETLIST_ORDER_FILE`] (F5.1). No setlist file changes.
    ///
    /// The setlist must be readable. An order file that cannot be used is
    /// replaced, except one with a newer `formatVersion`, which is refused
    /// with [`SetlistError::OrderVersion`] and left as it is. Setlists that
    /// cannot be read but are named in the stored order stay in it, after
    /// the others.
    pub fn move_setlist(&self, file: &str, to: usize) -> Result<(), SetlistError> {
        check_file_name(file)?;
        let folder = self.existing_setlists_folder(file)?;
        read_setlist(&folder, file)?;
        let mut listing = self.list_setlists()?;
        if let Some(error @ SetlistError::OrderVersion { .. }) = listing.order_error.take() {
            return Err(error);
        }
        let mut files: Vec<String> = listing.setlists.into_iter().map(|e| e.file).collect();
        let from = files
            .iter()
            .position(|listed| listed == file)
            .ok_or_else(|| SetlistError::Read {
                path: folder.join(file),
                source: io::Error::from(io::ErrorKind::NotFound),
            })?;
        if to >= files.len() {
            return Err(SetlistError::MoveOutOfRange {
                to,
                len: files.len(),
            });
        }
        let moved = files.remove(from);
        files.insert(to, moved);
        if let Ok(Some(stored)) = read_order(&folder) {
            for name in stored {
                let unreadable = listing.unreadable.iter().any(|u| u.file == name);
                if unreadable && !files.contains(&name) {
                    files.push(name);
                }
            }
        }
        write_order(&folder, &files)
    }

    /// Removes the setlist in `file` from the library's list (F5.1, L33) by
    /// moving its file to the system trash, where it can be restored; it is
    /// never deleted. `file` must be a setlist file name as for
    /// [`Library::load_setlist`], spelled exactly as the folder lists it
    /// ([`SetlistError::NotListed`] otherwise, also where the disk ignores
    /// case), and the file must resolve inside the [`SETLISTS_FOLDER`],
    /// links included, and be a regular file; otherwise nothing moves. It
    /// need not read as a setlist: a broken file, listed as unreadable, can
    /// be removed too, and is then named by its file. A stored order loses its entry and keeps the
    /// rest. When the file is in the trash but the order cannot be written,
    /// [`SetlistError::RemovedOrderNotUpdated`] says so; the name left in
    /// the order names no file, so it is ignored.
    pub fn remove_setlist(&self, file: &str) -> Result<SetlistEntry, SetlistError> {
        check_file_name(file)?;
        let folder = self.existing_setlists_folder(file)?;
        // Exactly as listed: on a disk that ignores case another spelling
        // would reach the same file under a name the order does not hold.
        if !listed_file_names(&folder)?.iter().any(|name| name == file) {
            return Err(SetlistError::NotListed {
                file: file.to_owned(),
            });
        }
        check_regular_inside(&folder, file)?;
        // A file that does not read as a setlist is removed all the same,
        // named by its file.
        let name = read_setlist(&folder, file).map_or_else(|_| file.to_owned(), |s| s.name);
        let path = folder.join(file);
        (self.trash)(&path).map_err(|source| SetlistError::Trash { path, source })?;
        let removed = SetlistEntry {
            name,
            file: file.to_owned(),
        };
        match update_order(&folder, |order| order.retain(|listed| listed != file)) {
            Ok(()) => Ok(removed),
            Err(error) => Err(SetlistError::RemovedOrderNotUpdated {
                removed,
                reason: error.to_string(),
            }),
        }
    }

    /// The name the [`SETLISTS_FOLDER`] lists for the setlist file `file`:
    /// `file` itself when it is listed so, else the listed name of the
    /// same file spelled otherwise (another case, or another Unicode
    /// normalization, on a file system that ignores them), or `None` when
    /// no listed setlist file is that file.
    pub fn listed_setlist_file(&self, file: &str) -> Result<Option<String>, SetlistError> {
        check_file_name(file)?;
        let Some(folder) = self.setlists_folder()? else {
            return Ok(None);
        };
        let listed = listed_file_names(&folder)?;
        if listed.iter().any(|name| name == file) {
            return Ok(Some(file.to_owned()));
        }
        let path = folder.join(file);
        if !entry_exists(&path)? {
            return Ok(None);
        }
        for name in listed {
            if same_entry(file, &path, &name, &folder.join(&name))? {
                return Ok(Some(name));
            }
        }
        Ok(None)
    }

    /// Reads the setlist file `file`, a file name inside the
    /// [`SETLISTS_FOLDER`] as [`SetlistEntry::file`] gives it. The file must
    /// resolve inside that folder, symlinks included, be a regular file (a
    /// FIFO is refused without being opened) and hold at most
    /// [`MAX_SETLIST_FILE_BYTES`]. Only its JSON shape is checked; call
    /// [`Setlist::validate`] for the rules.
    pub fn load_setlist(&self, file: &str) -> Result<Setlist, SetlistError> {
        check_file_name(file)?;
        let folder = self.existing_setlists_folder(file)?;
        read_setlist(&folder, file)
    }

    /// Writes `setlist` to the file its name maps to (see
    /// [setlist file names](Library#setlist-file-names)), creating the
    /// [`SETLISTS_FOLDER`] if needed, atomically (see
    /// [writing](Library#writing)).
    /// The file is replaced only when it holds a setlist with the same name;
    /// one holding another setlist is left as it was and
    /// [`SetlistError::FileTaken`] is returned. A blank name is refused.
    /// A new setlist goes at the end of a stored order; when that place
    /// cannot be written, the setlist is saved all the same and
    /// [`SetlistError::OrderNotUpdated`] says so.
    pub fn save_setlist(&self, setlist: &Setlist) -> Result<SetlistEntry, SetlistError> {
        if setlist.name.trim().is_empty() {
            return Err(SetlistEditError::EmptyName.into());
        }
        let file = file_name_for(&setlist.name);
        let json = setlist.to_json().map_err(|source| SetlistError::Format {
            path: self.root.join(SETLISTS_FOLDER).join(&file),
            source,
        })?;
        let folder = self.create_setlists_folder()?;
        let path = folder.join(&file);
        let created = !entry_exists(&path)?;
        if !created {
            let existing = read_setlist(&folder, &file)?;
            if existing.name != setlist.name {
                return Err(file_taken(&setlist.name, file));
            }
        }
        write_atomically(&path, &json)?;
        let saved = SetlistEntry {
            name: setlist.name.clone(),
            file,
        };
        if created {
            let placed = update_order(&folder, |order| {
                order.retain(|listed| *listed != saved.file);
                order.push(saved.file.clone());
            });
            return placed_or_saved(placed, saved);
        }
        Ok(saved)
    }

    /// Renames the setlist in `file` to `name` (F5.1), moving it to the file
    /// the new name maps to. A file already holding another setlist is never
    /// overwritten. Changing only the case of the name also works on a file
    /// system that ignores case. It keeps its place in a stored order; when
    /// that cannot be written, [`SetlistError::OrderNotUpdated`] says the
    /// rename was made all the same.
    pub fn rename_setlist(&self, file: &str, name: &str) -> Result<SetlistEntry, SetlistError> {
        check_file_name(file)?;
        let folder = self.existing_setlists_folder(file)?;
        let mut setlist = read_setlist(&folder, file)?;
        setlist.rename(name)?;
        let target = file_name_for(name);
        let source_path = folder.join(file);
        let target_path = folder.join(&target);
        let json = setlist.to_json().map_err(|source| SetlistError::Format {
            path: target_path.clone(),
            source,
        })?;
        let renamed = SetlistEntry {
            name: setlist.name,
            file: target.clone(),
        };
        if target == file {
            write_atomically(&source_path, &json)?;
            return Ok(renamed);
        }
        if entry_exists(&target_path)? {
            if !same_entry(file, &source_path, &target, &target_path)? {
                return Err(file_taken(name, target));
            }
            // Only the case differs, on a file system that ignores case:
            // rewrite the file, then give it the new spelling.
            write_atomically(&source_path, &json)?;
            fs::rename(&source_path, &target_path).map_err(|source| SetlistError::Write {
                path: target_path,
                source,
            })?;
            return placed_or_saved(keep_place(&folder, file, &target), renamed);
        }
        // Move first, then rewrite: an interruption in between leaves the
        // setlist under its new file name with its old name, never two
        // copies.
        fs::rename(&source_path, &target_path).map_err(|source| SetlistError::Write {
            path: target_path.clone(),
            source,
        })?;
        if let Err(error) = write_atomically(&target_path, &json) {
            let _ = fs::rename(&target_path, &source_path);
            return Err(error);
        }
        placed_or_saved(keep_place(&folder, file, &target), renamed)
    }

    /// Copies the setlist in `file` to a new setlist named `name` (F5.1), in
    /// the file that name maps to; an existing file there is never
    /// overwritten. The copy goes right after the original in a stored
    /// order; when that cannot be written, [`SetlistError::OrderNotUpdated`]
    /// says the copy was made all the same.
    pub fn duplicate_setlist(&self, file: &str, name: &str) -> Result<SetlistEntry, SetlistError> {
        check_file_name(file)?;
        let folder = self.existing_setlists_folder(file)?;
        let copy = read_setlist(&folder, file)?.duplicate(name)?;
        let target = file_name_for(name);
        let target_path = folder.join(&target);
        let json = copy.to_json().map_err(|source| SetlistError::Format {
            path: target_path.clone(),
            source,
        })?;
        if entry_exists(&target_path)? {
            return Err(file_taken(name, target));
        }
        write_atomically(&target_path, &json)?;
        let placed = update_order(&folder, |order| {
            order.retain(|listed| *listed != target);
            if let Some(original) = order.iter().position(|listed| listed == file) {
                order.insert(original + 1, target.clone());
            }
        });
        placed_or_saved(
            placed,
            SetlistEntry {
                name: copy.name,
                file: target,
            },
        )
    }

    /// The canonical library folder.
    pub(crate) fn canonical_root(&self) -> Result<PathBuf, SetlistError> {
        fs::canonicalize(&self.root).map_err(|source| SetlistError::Folder {
            path: self.root.clone(),
            source,
        })
    }

    /// The canonical setlists folder, or `None` when it does not exist.
    fn setlists_folder(&self) -> Result<Option<PathBuf>, SetlistError> {
        let root = self.canonical_root()?;
        match resolve_inside(&root, SETLISTS_FOLDER) {
            Ok(Some(folder)) => Ok(Some(folder)),
            Ok(None) => Err(SetlistError::OutsideLibrary {
                path: root.join(SETLISTS_FOLDER),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SetlistError::Folder {
                path: root.join(SETLISTS_FOLDER),
                source,
            }),
        }
    }

    /// The canonical setlists folder; when it does not exist, reading `file`
    /// from it fails as a missing file.
    fn existing_setlists_folder(&self, file: &str) -> Result<PathBuf, SetlistError> {
        self.setlists_folder()?.ok_or_else(|| SetlistError::Read {
            path: self.root.join(SETLISTS_FOLDER).join(file),
            source: io::Error::from(io::ErrorKind::NotFound),
        })
    }

    /// The canonical setlists folder, created first if it does not exist.
    fn create_setlists_folder(&self) -> Result<PathBuf, SetlistError> {
        let root = self.canonical_root()?;
        let folder = root.join(SETLISTS_FOLDER);
        match fs::create_dir(&folder) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(SetlistError::Folder {
                    path: folder,
                    source,
                });
            }
        }
        self.setlists_folder()?.ok_or_else(|| SetlistError::Folder {
            path: folder,
            source: io::Error::from(io::ErrorKind::NotFound),
        })
    }
}

/// Sorts entries by name ignoring case, then by name, then by file.
fn by_name(a: &SetlistEntry, b: &SetlistEntry) -> Ordering {
    a.name
        .to_lowercase()
        .cmp(&b.name.to_lowercase())
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.file.cmp(&b.file))
}

/// Whether `file` is a name [`Library::list_setlists`] lists: it ends in
/// [`SETLIST_EXTENSION`] after something, is not hidden, and is one plain
/// path component, with neither `/` nor `\` on any platform. On Windows a
/// `:` is refused too, since it would name a drive or an alternate data
/// stream; the names this library writes never contain one.
fn is_setlist_file_name(file: &str) -> bool {
    file.len() > SETLIST_EXTENSION.len()
        && file.ends_with(SETLIST_EXTENSION)
        && !file.starts_with('.')
        && !file.contains(['/', '\\'])
        && !(cfg!(windows) && file.contains(':'))
        && matches!(
            Path::new(file).components().collect::<Vec<_>>()[..],
            [Component::Normal(_)]
        )
}

fn check_file_name(file: &str) -> Result<(), SetlistError> {
    if is_setlist_file_name(file) {
        Ok(())
    } else {
        Err(SetlistError::InvalidFileName {
            file: file.to_owned(),
        })
    }
}

/// Entry errors after which [`setlist_file_names`] stops reading a folder,
/// so a folder whose reads keep failing cannot keep a listing going.
const MAX_ENTRY_ERRORS: usize = 100;

/// The setlist file names among `entries`, the names a folder lists, and
/// why the entries that could not be read could not: those are skipped,
/// so one bad entry hides no setlist (after [`MAX_ENTRY_ERRORS`] the rest
/// of the folder is left).
fn setlist_file_names(
    entries: impl Iterator<Item = io::Result<std::ffi::OsString>>,
) -> (Vec<String>, Vec<io::Error>) {
    let mut names = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        match entry {
            Ok(name) => {
                if let Ok(name) = name.into_string()
                    && is_setlist_file_name(&name)
                {
                    names.push(name);
                }
            }
            Err(error) => {
                errors.push(error);
                if errors.len() == MAX_ENTRY_ERRORS {
                    break;
                }
            }
        }
    }
    (names, errors)
}

/// The names `folder` (canonical) lists, the setlist files among them and
/// why the entries that could not be read could not ([`setlist_file_names`]);
/// only a folder that cannot be opened is an error.
fn read_setlist_names(folder: &Path) -> Result<(Vec<String>, Vec<io::Error>), SetlistError> {
    let entries = fs::read_dir(folder).map_err(|source| SetlistError::Folder {
        path: folder.to_owned(),
        source,
    })?;
    Ok(setlist_file_names(
        entries.map(|found| found.map(|entry| entry.file_name())),
    ))
}

/// The setlist file names the folder `folder` (canonical) lists, as
/// [`Library::list_setlists`] takes them: valid setlist file names in UTF-8.
fn listed_file_names(folder: &Path) -> Result<Vec<String>, SetlistError> {
    Ok(read_setlist_names(folder)?.0)
}

/// Checks that `file` (already checked by [`check_file_name`]) resolves
/// inside `folder` (canonical), links included, and is a regular file,
/// without opening it, so a FIFO cannot block.
fn check_regular_inside(folder: &Path, file: &str) -> Result<(), SetlistError> {
    let path = folder.join(file);
    let resolved = match resolve_inside(folder, file) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return Err(SetlistError::OutsideLibrary { path }),
        Err(source) => return Err(SetlistError::Read { path, source }),
    };
    match fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(SetlistError::NotAFile { path }),
        Err(source) => Err(SetlistError::Read { path, source }),
    }
}

/// Reads and parses the setlist file `file` (already checked by
/// [`check_file_name`]) inside `folder` (canonical).
fn read_setlist(folder: &Path, file: &str) -> Result<Setlist, SetlistError> {
    let json = read_folder_file(folder, file)?;
    Setlist::from_json(&json).map_err(|source| SetlistError::Format {
        path: folder.join(file),
        source,
    })
}

/// Reads the file `file` inside `folder` (canonical) as text: it must
/// resolve inside `folder`, symlinks included, be a regular file (a FIFO is
/// refused without being opened) and hold at most
/// [`MAX_SETLIST_FILE_BYTES`].
pub(crate) fn read_folder_file(folder: &Path, file: &str) -> Result<String, SetlistError> {
    let path = folder.join(file);
    read_confined(folder, file, MAX_SETLIST_FILE_BYTES).map_err(|error| match error {
        ConfinedReadError::Outside => SetlistError::OutsideLibrary { path },
        ConfinedReadError::NotAFile => SetlistError::NotAFile { path },
        ConfinedReadError::TooLarge => SetlistError::TooLarge {
            path,
            max_bytes: MAX_SETLIST_FILE_BYTES,
        },
        ConfinedReadError::NotUtf8 => SetlistError::Read {
            path,
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            ),
        },
        ConfinedReadError::Io(source) => SetlistError::Read { path, source },
    })
}

/// `saved`, or [`SetlistError::OrderNotUpdated`] when `placed` says its
/// place in the stored order could not be written.
fn placed_or_saved(
    placed: Result<(), SetlistError>,
    saved: SetlistEntry,
) -> Result<SetlistEntry, SetlistError> {
    match placed {
        Ok(()) => Ok(saved),
        Err(error) => Err(SetlistError::OrderNotUpdated {
            saved,
            reason: error.to_string(),
        }),
    }
}

/// After the setlist in `file` was renamed to `target`, puts `target` in
/// its place in the stored order.
fn keep_place(folder: &Path, file: &str, target: &str) -> Result<(), SetlistError> {
    update_order(folder, |order| {
        if !order.iter().any(|listed| listed == file) {
            return;
        }
        order.retain(|listed| listed != target);
        if let Some(place) = order.iter().position(|listed| listed == file) {
            order[place] = target.to_owned();
        }
    })
}

/// Whether anything, a dangling symlink included, has the path `path`.
fn entry_exists(path: &Path) -> Result<bool, SetlistError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(SetlistError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Whether the existing `target` is `source` spelled with another case or
/// Unicode normalization, as a file system that ignores them sees it.
///
/// On unix the file's identity (device and inode) decides alone: HFS+ and
/// exFAT list names decomposed (NFD), so comparing the listed name with a
/// freshly built one as strings would refuse a case-only rename there.
/// Elsewhere identity falls back to size and modification time, so the
/// names must also match ignoring case.
fn same_entry(
    source: &str,
    source_path: &Path,
    target: &str,
    target_path: &Path,
) -> Result<bool, SetlistError> {
    if !cfg!(unix) && source.to_lowercase() != target.to_lowercase() {
        return Ok(false);
    }
    let metadata = |path: &Path| {
        fs::symlink_metadata(path).map_err(|source| SetlistError::Read {
            path: path.to_owned(),
            source,
        })
    };
    Ok(same_file(&metadata(source_path)?, &metadata(target_path)?))
}

fn file_taken(name: &str, file: String) -> SetlistError {
    SetlistError::FileTaken {
        name: name.to_owned(),
        file,
    }
}

/// Writes `contents` to a temporary file beside `path`, flushes it to disk,
/// and renames it over `path`; on failure the temporary file is removed and
/// an existing file at `path` is left as it was.
pub(crate) fn write_atomically(path: &Path, contents: &str) -> Result<(), SetlistError> {
    write_file_atomically(path, contents).map_err(|source| SetlistError::Write {
        path: path.to_owned(),
        source,
    })
}

/// The file name the setlist `name` is stored under; see
/// [setlist file names](Library#setlist-file-names).
fn file_name_for(name: &str) -> String {
    let mut stem = String::new();
    for character in name.chars() {
        let mut buffer = [0; 4];
        let escaped;
        let piece: &str = if needs_escape(character, stem.is_empty()) {
            escaped = format!("%{:02X}", u32::from(character));
            &escaped
        } else {
            character.encode_utf8(&mut buffer)
        };
        if stem.len() + piece.len() > MAX_FILE_STEM_BYTES {
            break;
        }
        stem.push_str(piece);
    }
    if is_windows_reserved(&stem) {
        // Every reserved name starts with an ASCII letter.
        let first = stem.remove(0);
        stem.insert_str(0, &format!("%{:02X}", u32::from(first)));
    }
    stem + SETLIST_EXTENSION
}

/// Whether `character` is written as `%XX`; `first` is whether it starts the
/// file name.
fn needs_escape(character: char, first: bool) -> bool {
    matches!(
        character,
        '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%'
    ) || character.is_ascii_control()
        || (first && character == '.')
}

/// Whether Windows reserves `stem` for a device: the part before its first
/// dot, without trailing spaces, is a reserved name in any case.
fn is_windows_reserved(stem: &str) -> bool {
    let base = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_entry_that_cannot_be_read_hides_no_setlist() {
        let entries = [
            Ok("A.setlist.json".into()),
            Err(io::Error::other("the disk said no")),
            Ok("B.setlist.json".into()),
            Ok("notes.txt".into()),
            Ok(".hidden.setlist.json".into()),
            Ok(SETLIST_ORDER_FILE.into()),
        ];

        let (names, errors) = setlist_file_names(entries.into_iter());

        assert_eq!(names, ["A.setlist.json", "B.setlist.json"]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].to_string(), "the disk said no");
    }

    #[test]
    fn a_folder_whose_entries_keep_failing_is_left() {
        let (names, errors) =
            setlist_file_names(std::iter::repeat_with(|| Err(io::Error::other("again"))));
        assert!(names.is_empty());
        assert_eq!(errors.len(), MAX_ENTRY_ERRORS);
    }
}
