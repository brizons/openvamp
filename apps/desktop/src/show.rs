//! What the Perform screen plays: one song folder, or a setlist from a
//! library folder (S4). Either one opens in place of the other with the
//! same care as a song in place of a song ([`crate::song::replace`]): the
//! pick and the device are checked before what plays stops, and the device
//! opens only once the old show has closed it.
//!
//! Preparing a pick reads its files ([`Source::prepare`]), which a slow,
//! sleeping or unplugged volume can hold up for seconds, so the screen runs
//! it on its background executor while what plays goes on, Panic
//! included, and then hands the result to [`replace_prepared`].

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use app::{
    AudioOutput, Command, OpenError, OutputCheck, PreparedSetlist, PreparedSong, RuntimeError,
    Session, SessionOptions, SetlistCommand, SetlistCommandError, SetlistOpenError, SetlistSession,
    SetlistSnapshot, Snapshot, SongInfo,
};
use domain::{BusSettings, MixFile, Routing, SetlistItem};
use library::{Library, SETLIST_EXTENSION, SETLISTS_FOLDER, SetlistError};

use crate::action::{UiAction, command_for, setlist_command_for};
use crate::catalog::Catalog;
use crate::editor::{Edit, SetlistEditor};
use crate::reorder::{Half, drop_index, drop_slot};
use crate::song::{Closed, Song, stop_and_wait};
use crate::strip::SetlistView;
use crate::view::open_error_lines;

/// What the screen can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A song folder.
    Song(PathBuf),
    /// A setlist file of a library folder.
    Setlist {
        /// The library folder: the one holding the [`SETLISTS_FOLDER`].
        library: PathBuf,
        /// The file name inside the [`SETLISTS_FOLDER`], as
        /// [`Library::load_setlist`] takes it.
        file: String,
    },
}

impl Source {
    /// The setlist file at `path`: its library is the folder holding the
    /// [`SETLISTS_FOLDER`] it is in (the folder's name may be in any case).
    /// A relative path counts from the current folder, and `.` and `..`
    /// are resolved by name, without following symlinks, as `play-cli`
    /// does. `None` when `path` is not inside such a folder.
    pub fn setlist_file(path: &Path) -> Option<Self> {
        // A path ending in `..` names a folder, never a setlist file.
        if path.ends_with(Component::ParentDir.as_os_str()) {
            return None;
        }
        let path = resolve_dots(&std::path::absolute(path).ok()?);
        let folder = path.parent()?;
        if !folder.file_name().is_some_and(is_setlists_folder) {
            return None;
        }
        Some(Self::Setlist {
            library: folder.parent()?.to_owned(),
            file: path.file_name()?.to_str()?.to_owned(),
        })
    }

    /// What `path` opens: a file whose name ends in
    /// [`library::SETLIST_EXTENSION`] is a setlist ([`Self::setlist_file`]),
    /// anything else a song folder. Why not, for a setlist file outside a
    /// [`SETLISTS_FOLDER`].
    pub fn for_path(path: &Path) -> Result<Self, String> {
        let is_setlist = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(SETLIST_EXTENSION));
        if !is_setlist {
            return Ok(Self::Song(path.to_owned()));
        }
        Self::setlist_file(path).ok_or_else(|| {
            format!(
                "{} is not in the {SETLISTS_FOLDER} folder of a library folder, so its songs cannot be found",
                path.display()
            )
        })
    }

    /// The setlist file picked at `path`; why not, for any other file.
    pub fn picked_setlist(path: &Path) -> Result<Self, String> {
        match Self::for_path(path)? {
            Self::Song(_) => Err(format!(
                "{} is not a setlist: pick a file ending in {SETLIST_EXTENSION} in the {SETLISTS_FOLDER} folder of a library folder",
                path.display()
            )),
            setlist => Ok(setlist),
        }
    }

    /// The folder or file, for messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Song(dir) => dir.display().to_string(),
            Self::Setlist { library, file } => library
                .join(SETLISTS_FOLDER)
                .join(file)
                .display()
                .to_string(),
        }
    }

    /// Checks and prepares the show with no device ([`Session::prepare`],
    /// [`SetlistSession::prepare`]); a setlist also lists the library's
    /// songs to name its items. It reads the song folder or the setlist and
    /// its first song, so it waits for the volume: the screen runs it off
    /// its UI thread.
    pub fn prepare(&self, options: &SessionOptions) -> Result<Prepared, OpenFailure> {
        self.prepare_show(options).map(Prepared)
    }

    fn prepare_show(&self, options: &SessionOptions) -> Result<PreparedShow, OpenFailure> {
        match self {
            Self::Song(dir) => Session::prepare(dir, options.clone())
                .map(|prepared| PreparedShow::Song(dir.clone(), Box::new(prepared)))
                .map_err(OpenFailure::Song),
            Self::Setlist { library, file } => {
                // Kept canonical, so comparing it with the Library
                // screen's folder needs no file system call.
                let library = std::fs::canonicalize(library).map_err(|source| {
                    OpenFailure::Setlist(SetlistOpenError::Library(SetlistError::Folder {
                        path: library.clone(),
                        source,
                    }))
                })?;
                let catalog = match Library::new(&library).list_songs() {
                    Ok(listing) => Catalog::from_listing(&listing),
                    Err(error) => Catalog::unavailable(format!(
                        "The library's songs could not be listed, so items show their folder names: {error}"
                    )),
                };
                // Kept as the folder lists it, whatever case it was given
                // in where the disk ignores case, so the Library screen
                // knows it as the setlist on Perform.
                let file = Library::new(&library)
                    .listed_setlist_file(file)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| file.clone());
                let prepared =
                    SetlistSession::prepare(&Library::new(&library), &file, options.clone())
                        .map_err(OpenFailure::Setlist)?;
                Ok(PreparedShow::Setlist {
                    library,
                    file,
                    prepared: Box::new(prepared),
                    catalog,
                })
            }
        }
    }
}

/// Why a show did not open.
#[derive(Debug)]
pub enum OpenFailure {
    /// The song folder or its device.
    Song(OpenError),
    /// The setlist, its first item or its device.
    Setlist(SetlistOpenError),
}

impl OpenFailure {
    /// Why, one line per problem, as the `app` API words them: every
    /// problem of an invalid song package, and every reason each item of a
    /// setlist that cannot play at all cannot play.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Self::Song(error) | Self::Setlist(SetlistOpenError::Open(error)) => {
                open_error_lines(error)
            }
            Self::Setlist(error @ SetlistOpenError::NothingPlayable { problems, .. }) => {
                let mut lines = vec![error.to_string()];
                for (index, reasons) in problems.iter().enumerate() {
                    for reason in reasons {
                        lines.push(format!("item {} cannot play: {reason}", index + 1));
                    }
                }
                lines
            }
            Self::Setlist(error) => vec![error.to_string()],
        }
    }
}

/// A show checked and loaded with no device ([`Source::prepare`]), ready
/// to take the place of what plays ([`replace_prepared`]). It can move to
/// another thread.
pub struct Prepared(PreparedShow);

/// A show checked with no device.
enum PreparedShow {
    Song(PathBuf, Box<PreparedSong>),
    Setlist {
        library: PathBuf,
        file: String,
        prepared: Box<PreparedSetlist>,
        catalog: Catalog,
    },
}

impl Prepared {
    fn check(&self, output: &dyn AudioOutput) -> OutputCheck {
        self.0.check(output)
    }

    fn start(self, output: Box<dyn AudioOutput>) -> Result<Show, OpenError> {
        self.0.start(output)
    }
}

impl PreparedShow {
    fn check(&self, output: &dyn AudioOutput) -> OutputCheck {
        match self {
            Self::Song(_, prepared) => prepared.check(output),
            Self::Setlist { prepared, .. } => prepared.check(output),
        }
    }

    fn start(self, output: Box<dyn AudioOutput>) -> Result<Show, OpenError> {
        match self {
            Self::Song(dir, prepared) => {
                Song::start(&dir, *prepared, output).map(|song| Show::Song(Box::new(song)))
            }
            Self::Setlist {
                library,
                file,
                prepared,
                catalog,
            } => {
                let mut session = prepared.start(output)?;
                let snapshot = session.poll();
                let known = items_of(&session);
                Ok(Show::Setlist(Box::new(LoadedSetlist {
                    library,
                    file,
                    session,
                    snapshot,
                    catalog,
                    refused: None,
                    insertion: None,
                    known,
                    unsaved: None,
                    saves: Vec::new(),
                })))
            }
        }
    }
}

/// A setlist loaded on a device: where it came from, its session, the last
/// snapshot, the library's songs that name its items, and the last
/// setlist control it refused.
pub struct LoadedSetlist {
    /// The library folder.
    pub library: PathBuf,
    /// The setlist file inside its [`SETLISTS_FOLDER`].
    pub file: String,
    /// The session playing it.
    pub session: SetlistSession,
    /// The session's state at the last poll.
    pub snapshot: SetlistSnapshot,
    /// The library's songs, to name the items.
    pub catalog: Catalog,
    /// The last previous, next, select or move the setlist refused, until
    /// one is accepted.
    pub refused: Option<SetlistCommandError>,
    /// The tile dragged along the strip and the gap it would land in, while
    /// dropping there would move it.
    insertion: Option<(usize, usize)>,
    /// The items the setlist file was last known to hold, in its order: as
    /// opened, or as a move on the strip last saved them.
    known: Vec<SetlistItem>,
    /// Why the order last moved on the strip is not in the file, though it
    /// plays; or the file's place in the stored order that could not be
    /// written.
    unsaved: Option<String>,
    /// The setlist files a move on the strip wrote since the last
    /// [`Self::take_saves`], as (file before, file after).
    saves: Vec<(String, String)>,
}

impl LoadedSetlist {
    /// Opens the setlist `file` of the library folder `library` on
    /// `output`, at its first playable item, stopped at its start.
    pub fn open(
        library: &Path,
        file: &str,
        options: &SessionOptions,
        output: Box<dyn AudioOutput>,
    ) -> Result<Self, OpenFailure> {
        let source = Source::Setlist {
            library: library.to_owned(),
            file: file.to_owned(),
        };
        match source.prepare(options)?.start(output) {
            Ok(Show::Setlist(setlist)) => Ok(*setlist),
            Ok(Show::Song(_)) => unreachable!("a setlist source prepares a setlist"),
            Err(error) => Err(OpenFailure::Setlist(SetlistOpenError::Open(error))),
        }
    }

    /// Moves the session on and keeps its snapshot.
    ///
    /// A refused previous, next or select is forgotten once the setlist
    /// moves to another item by itself (an AutoLink, a Crossfade, Cue
    /// next), since it no longer says anything about where the setlist is.
    pub fn poll(&mut self) {
        let before = self.snapshot.setlist.current;
        self.snapshot = self.session.poll();
        if self.snapshot.setlist.current != before {
            self.refused = None;
        }
    }

    /// Sends the command `action` stands for. A refused item command shows
    /// as the snapshot's last error; a refused previous, next or select is
    /// kept in [`Self::refused`] until one is accepted.
    pub fn send(&mut self, action: UiAction) {
        let Some(command) = setlist_command_for(action, &self.snapshot, self.session.song()) else {
            return;
        };
        match self.session.send(command) {
            Ok(()) => {
                if !matches!(command, SetlistCommand::Item(_)) {
                    self.refused = None;
                }
            }
            // The snapshot's last error says why.
            Err(SetlistCommandError::Runtime(_)) => {}
            Err(error) => self.refused = Some(error),
        }
    }

    /// What the screen shows of the setlist.
    pub fn view(&self) -> SetlistView {
        let mut view = SetlistView::new(
            self.session.info(),
            &self.snapshot,
            &self.catalog,
            self.refused.as_ref(),
        );
        view.insertion = self.insertion.map(|(_, slot)| slot);
        view.problems.extend(self.unsaved.clone());
        view
    }

    /// A drag of a strip tile starts: no line shows until it is over a
    /// tile, whatever an earlier drag that never dropped left behind.
    pub fn start_drag(&mut self) {
        self.insertion = None;
    }

    /// A drag of tile `from` is over tile `over`, in its `half` (the left
    /// one is [`Half::Upper`]): the line shows where it would land, or
    /// nowhere when dropping there would move nothing.
    pub fn drag_over(&mut self, from: usize, over: usize, half: Half) {
        let slot = drop_slot(over, half);
        let items = self.session.info().items.len();
        self.insertion = drop_index(from, slot, items).map(|_| (from, slot));
    }

    /// The drag of tile `from` was dropped on the strip: it moves to where
    /// the line shows ([`Self::move_item`]), or nowhere without a line of
    /// its own.
    pub fn drop_dragged(&mut self, from: usize) {
        // Taken either way: a line worked out for another tile never lasts.
        let Some((shown, slot)) = self.insertion.take() else {
            return;
        };
        let items = self.session.info().items.len();
        if shown == from
            && let Some(to) = drop_index(from, slot, items)
        {
            self.move_item(from, to);
        }
    }

    /// The drag ended anywhere else, or left the strip: nothing moves and
    /// the line goes. Whether a line was showing.
    pub fn cancel_drag(&mut self) -> bool {
        self.insertion.take().is_some()
    }

    /// Moves item `from` to `to` while the setlist plays (L46,
    /// [`SetlistCommand::Move`]), then writes the order playing to the
    /// setlist file through the Library screen's editor. A move the session
    /// refuses shows as refused, until a control is accepted or the setlist
    /// moves on, and changes nothing. A move the file cannot take still
    /// plays, and the problems say it was not saved: while the file holds
    /// what it was last known to hold, the next move saves the whole order
    /// (after a write that failed, say); a file changed since by someone
    /// else is left as it is.
    pub fn move_item(&mut self, from: usize, to: usize) {
        if let Err(error) = self.session.send(SetlistCommand::Move { from, to }) {
            self.refused = Some(error);
            return;
        }
        self.refused = None;
        if from == to {
            return;
        }
        // The snapshot's indexes follow their items at once.
        self.snapshot = self.session.poll();
        let order = items_of(&self.session);
        match save_order(&self.library, &self.file, &self.known, &order) {
            Ok((file, warning)) => {
                self.saves.push((self.file.clone(), file.clone()));
                self.file = file;
                self.known = order;
                self.unsaved = warning;
            }
            Err(reason) => {
                self.unsaved = Some(format!(
                    "The new order plays but was not saved to {}: {reason}",
                    self.file
                ));
            }
        }
    }

    /// The setlist files a move on the strip wrote since the last call, as
    /// (file before, file after): the Library screen reads them again.
    pub fn take_saves(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.saves)
    }
}

/// The items `session` plays, in its order.
fn items_of(session: &SetlistSession) -> Vec<SetlistItem> {
    session
        .info()
        .items
        .iter()
        .map(|item| item.item.clone())
        .collect()
}

/// Writes `order` to the setlist `file` of the library folder `library`,
/// which must still hold the items `known`; returns the file the setlist is
/// in afterwards (a file named by hand moves to the one its name maps to)
/// and a warning when its place in the stored order could not be written.
fn save_order(
    library: &Path,
    file: &str,
    known: &[SetlistItem],
    order: &[SetlistItem],
) -> Result<(String, Option<String>), String> {
    let mut editor =
        SetlistEditor::open(Library::new(library), file).map_err(|error| error.to_string())?;
    if editor.setlist().items != known {
        return Err(
            "the file changed since the setlist was opened; open it again to play those changes"
                .to_owned(),
        );
    }
    let warning = editor
        .apply(Edit::Order(order.to_vec()))
        .map_err(|error| error.to_string())?;
    Ok((editor.file().to_owned(), warning))
}

/// What plays: a song or a setlist.
pub enum Show {
    /// One song folder.
    Song(Box<Song>),
    /// A setlist.
    Setlist(Box<LoadedSetlist>),
}

impl Show {
    /// The current song's state at the last poll (a setlist's current
    /// item).
    pub fn snapshot(&self) -> &Snapshot {
        match self {
            Self::Song(song) => &song.snapshot,
            Self::Setlist(setlist) => &setlist.snapshot.song,
        }
    }

    /// The current song (a setlist's current item).
    pub fn song(&self) -> &SongInfo {
        match self {
            Self::Song(song) => song.session.song(),
            Self::Setlist(setlist) => setlist.session.song(),
        }
    }

    /// The setlist, when one plays.
    pub fn setlist(&self) -> Option<&LoadedSetlist> {
        match self {
            Self::Song(_) => None,
            Self::Setlist(setlist) => Some(setlist),
        }
    }

    /// Sends `command`, a mixer command, to the song (a setlist's current
    /// item). A refusal also shows as the snapshot's last error.
    pub fn send_mixer(&mut self, command: Command) -> Result<(), RuntimeError> {
        match self {
            Self::Song(song) => song.session.send(command),
            Self::Setlist(setlist) => match setlist.session.send(SetlistCommand::Item(command)) {
                Err(SetlistCommandError::Runtime(error)) => Err(error),
                // Only an item command's own refusal comes back for one.
                _ => Ok(()),
            },
        }
    }

    /// The song's mix file and the folder it goes in, with the mix playing
    /// as its arrangement's; `None` for a click-only tile.
    pub fn mix_file(&self) -> Option<(PathBuf, MixFile)> {
        match self {
            Self::Song(song) => song.session.mix_file(),
            Self::Setlist(setlist) => setlist.session.mix_file(),
        }
    }

    /// The mix file was written: the mix is no longer changed.
    pub fn mix_saved(&mut self) {
        match self {
            Self::Song(song) => song.session.mix_saved(),
            Self::Setlist(setlist) => setlist.session.mix_saved(),
        }
    }

    /// The global bus and master settings playing.
    pub fn bus_settings(&self) -> &BusSettings {
        match self {
            Self::Song(song) => song.session.bus_settings(),
            Self::Setlist(setlist) => setlist.session.bus_settings(),
        }
    }

    /// The bus settings were saved.
    pub fn bus_settings_saved(&mut self) {
        match self {
            Self::Song(song) => song.session.bus_settings_saved(),
            Self::Setlist(setlist) => setlist.session.bus_settings_saved(),
        }
    }

    /// Plays with `routing` from now on ([`app::Session::set_routing`]).
    pub fn set_routing(&mut self, routing: Routing) -> Result<(), RuntimeError> {
        match self {
            Self::Song(song) => song.session.set_routing(routing),
            Self::Setlist(setlist) => setlist.session.set_routing(routing),
        }
    }

    /// Plays with buffers of `frames` from now on
    /// ([`app::Session::set_buffer_frames`]).
    pub fn set_buffer_frames(&mut self, frames: u32) -> Result<(), RuntimeError> {
        match self {
            Self::Song(song) => song.session.set_buffer_frames(frames),
            Self::Setlist(setlist) => setlist.session.set_buffer_frames(frames),
        }
    }

    /// The setlist, when one plays, to change.
    pub fn setlist_mut(&mut self) -> Option<&mut LoadedSetlist> {
        match self {
            Self::Song(_) => None,
            Self::Setlist(setlist) => Some(setlist),
        }
    }

    /// Moves the session on and keeps its snapshot.
    pub fn poll(&mut self) {
        match self {
            Self::Song(song) => song.poll(),
            Self::Setlist(setlist) => setlist.poll(),
        }
    }

    /// Sends the command `action` stands for; a single song ignores the
    /// setlist controls, and a refused command shows as the snapshot's last
    /// error (or, for a setlist control, in the setlist's problems).
    pub fn send(&mut self, action: UiAction) {
        match self {
            Self::Song(song) => {
                if let Some(command) = command_for(action, &song.snapshot, song.session.song()) {
                    let _ = song.session.send(command);
                }
            }
            Self::Setlist(setlist) => setlist.send(action),
        }
    }

    /// The setlist file `from` of the library folder `library` moved to
    /// `to` (renamed, or moved to its name's file): a setlist playing from
    /// it now names the new file.
    pub fn follow_move(&mut self, library: &Path, from: &str, to: &str) {
        if let Self::Setlist(setlist) = self
            && setlist.library == library
            && setlist.file == from
        {
            setlist.file = to.to_owned();
        }
    }

    /// Where it was opened from, to open it again.
    pub fn source(&self) -> Source {
        match self {
            Self::Song(song) => Source::Song(song.dir.clone()),
            Self::Setlist(setlist) => Source::Setlist {
                library: setlist.library.clone(),
                file: setlist.file.clone(),
            },
        }
    }
}

/// Stops `show` with its ramp ([`stop_and_wait`]), then closes its session,
/// which closes its device. A setlist reads as its name.
pub fn close(show: Show, sleep: impl FnMut(Duration)) -> Closed {
    match show {
        Show::Song(song) => crate::song::close(*song, sleep),
        Show::Setlist(mut setlist) => {
            let closing = stop_and_wait(&mut setlist.session, sleep);
            let closed = Closed {
                title: setlist.session.info().name.clone(),
                dir: setlist.library.clone(),
                closing,
            };
            drop(setlist);
            closed
        }
    }
}

/// What [`replace`] did once the pick had prepared.
#[derive(Debug)]
pub struct Replaced {
    /// What was picked.
    pub source: Source,
    /// What was loaded before, how it closed, and where it came from.
    pub previous: Option<(Closed, Source)>,
    /// Why the pick's device did not open although its check went ahead,
    /// if it did not. What plays is then the previous show, opened again
    /// and stopped at the start, or nothing.
    pub start_error: Option<OpenError>,
    /// Why the previous show could not be opened again after that.
    pub reopen_error: Option<OpenFailure>,
}

impl Replaced {
    /// The show to open again: the previous one, when the pick's device did
    /// not start ([`reopen_previous`]).
    pub fn to_reopen(&self) -> Option<&Source> {
        self.start_error.as_ref()?;
        self.previous.as_ref().map(|(_, source)| source)
    }

    /// What to show and report, one line each; empty when the previous show
    /// stopped with its ramp and the pick opened.
    pub fn problems(&self) -> Vec<String> {
        let mut problems: Vec<String> = self
            .previous
            .iter()
            .filter_map(|(closed, _)| closed.problem())
            .collect();
        if let Some(error) = &self.start_error {
            problems.push(format!(
                "Could not start {} on the audio device: {error}",
                self.source.describe()
            ));
            if let Some((previous, _)) = &self.previous {
                match &self.reopen_error {
                    None => problems.push(format!(
                        "\"{}\" is loaded again, stopped at the start.",
                        previous.title
                    )),
                    Some(error) => {
                        let mut lines = error.lines().into_iter();
                        let first = lines.next().unwrap_or_default();
                        problems.push(format!(
                            "\"{}\" could not be loaded again: {first}",
                            previous.title
                        ));
                        problems.extend(lines);
                    }
                }
            }
        }
        problems
    }
}

/// Opens `source` in place of `current`, in the order
/// [`crate::song::replace`] follows, for a song or a setlist on either
/// side: the pick is prepared and the device asked first, and a pick that
/// does not open or that the device refuses leaves `current` playing; then
/// `current` stops with its ramp and closes its device; then the pick opens
/// the device, and when that fails the previous show opens again from its
/// source, stopped at the start.
pub fn replace(
    current: &mut Option<Show>,
    source: &Source,
    options: &SessionOptions,
    mut output: impl FnMut() -> Box<dyn AudioOutput>,
    sleep: impl FnMut(Duration),
) -> Result<Replaced, OpenFailure> {
    let prepared = source.prepare(options)?;
    let mut replaced = replace_prepared(current, source, prepared, output(), sleep)?;
    if let Some(previous) = replaced.to_reopen().cloned() {
        let prepared = previous.prepare(options);
        reopen_previous(current, &mut replaced, prepared, output);
    }
    Ok(replaced)
}

/// [`replace`] once `source` has prepared ([`Source::prepare`]), with
/// `device` to open: the device is asked first, and a device that refuses
/// leaves `current` playing; then `current` stops with its ramp and closes
/// its device, and the pick opens `device`. When that fails, nothing plays
/// and [`Replaced::to_reopen`] names the previous show, for the caller to
/// prepare again and hand to [`reopen_previous`].
pub fn replace_prepared(
    current: &mut Option<Show>,
    source: &Source,
    prepared: Prepared,
    device: Box<dyn AudioOutput>,
    sleep: impl FnMut(Duration),
) -> Result<Replaced, OpenFailure> {
    if let OutputCheck::Refused(error) = prepared.check(device.as_ref()) {
        return Err(failure(source, error.into()));
    }
    let previous = current.take().map(|show| {
        let previous_source = show.source();
        (close(show, sleep), previous_source)
    });
    let mut replaced = Replaced {
        source: source.clone(),
        previous,
        start_error: None,
        reopen_error: None,
    };
    match prepared.start(device) {
        Ok(show) => *current = Some(show),
        Err(error) => replaced.start_error = Some(error),
    }
    Ok(replaced)
}

/// Opens the previous show again, stopped at the start, after the pick's
/// device did not start ([`Replaced::to_reopen`]): `prepared` is its
/// preparation, and `output` gives the device. Why it could not goes to
/// [`Replaced::reopen_error`].
pub fn reopen_previous(
    current: &mut Option<Show>,
    replaced: &mut Replaced,
    prepared: Result<Prepared, OpenFailure>,
    output: impl FnOnce() -> Box<dyn AudioOutput>,
) {
    let Some(previous) = replaced.to_reopen().cloned() else {
        return;
    };
    match prepared.and_then(|prepared| {
        prepared
            .start(output())
            .map_err(|error| failure(&previous, error))
    }) {
        Ok(show) => *current = Some(show),
        Err(error) => replaced.reopen_error = Some(error),
    }
}

/// `error` from opening `source`'s device.
fn failure(source: &Source, error: OpenError) -> OpenFailure {
    match source {
        Source::Song(_) => OpenFailure::Song(error),
        Source::Setlist { .. } => OpenFailure::Setlist(SetlistOpenError::Open(error)),
    }
}

/// `path` (absolute) with `.` dropped and each `..` taking off the name
/// before it; `..` at the root stays at the root.
fn resolve_dots(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved.parent().is_some() {
                    resolved.pop();
                }
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// Whether `name` is the setlists folder's name, in any case.
fn is_setlists_folder(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(SETLISTS_FOLDER))
}
