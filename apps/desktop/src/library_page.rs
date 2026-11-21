//! The Library & setlists screen without a window (S4, F5.1–F5.5): the
//! setlists of a library folder, the editor of the one selected, and the
//! library's songs to add to it. [`LibraryPage::act`] carries out what the
//! screen's controls ask for; [`LibraryPage::view`] says what it shows.

use std::path::{Path, PathBuf};

use domain::{
    ClickItem, DEFAULT_ARRANGEMENT_ID, MAX_BPM, MIN_BPM, SetlistItem, SetlistValidationError,
    SongItem, TimeSignature, Transition,
};
use library::{ArrangementEntry, Library, SetlistError, SongEntry, TrashFn};

use crate::catalog::Catalog;
use crate::editor::{Edit, SetlistEditor};
use crate::line_edit::{LineEdit, LineKey, LineOutcome};
use crate::reorder::{Half, drop_index, drop_slot};
use crate::shelf::Shelf;

/// The label of an armed remove control. It does not repeat the name: the
/// setlist's name is in the editor's header (an unreadable file's in the
/// line above its control), and a long label next to the header's other
/// buttons would leave the name no room.
pub const ARMED_LABEL: &str = "Click again to remove";

/// The line above the editor's name while its setlist is open on the
/// Perform screen, which plays it as it was when opened.
pub const ON_PERFORM: &str = "On Perform · changes play the next time it opens";

/// The meters a click-only tile offers, as (numerator, denominator).
pub const METERS: [(u32, u32); 8] = [
    (2, 4),
    (3, 4),
    (4, 4),
    (5, 4),
    (6, 8),
    (7, 8),
    (9, 8),
    (12, 8),
];

/// A control of the Library screen; indexes are rows of the setlist list,
/// of the editor, or of the song list as [`LibraryView`] shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryAction {
    /// Opens this setlist of the list in the editor.
    SelectSetlist(usize),
    /// Creates a setlist and starts naming it.
    NewSetlist,
    /// Starts renaming the setlist in the editor.
    RenameSetlist,
    /// Copies the setlist in the editor and starts naming the copy.
    DuplicateSetlist,
    /// Moves this setlist of the list one place up.
    MoveSetlistUp(usize),
    /// Moves this setlist of the list one place down.
    MoveSetlistDown(usize),
    /// Moves the item at `from` to index `to`: a drag dropped in the
    /// editor ([`LibraryPage::drop_dragged`]).
    DropItem {
        /// The item dragged.
        from: usize,
        /// The index it ends up at.
        to: usize,
    },
    /// Moves the setlist at row `from` of the list to row `to`: a drag
    /// dropped in the list.
    DropSetlist {
        /// The setlist dragged.
        from: usize,
        /// The row it ends up at.
        to: usize,
    },
    /// Removes the setlist in the editor: the first press arms it, the
    /// second moves its file to the system trash; any other action
    /// disarms it.
    RemoveSetlist,
    /// Removes this setlist file of those that cannot be read, in two
    /// presses like [`Self::RemoveSetlist`].
    RemoveUnreadable(usize),
    /// Shows this item's details in the editor (again: hides them).
    SelectRow(usize),
    /// Adds the song of this row of the song list at the end.
    AddSong(usize),
    /// Adds a click-only tile at the end.
    AddClick,
    /// Removes the item.
    Remove(usize),
    /// Moves the item one place up.
    MoveUp(usize),
    /// Moves the item one place down.
    MoveDown(usize),
    /// Chooses the song's arrangement, by its place in the song.
    Arrangement(usize, usize),
    /// Starts the song one position earlier.
    StartEarlier(usize),
    /// Starts the song one position later.
    StartLater(usize),
    /// Ends the song one position earlier.
    EndEarlier(usize),
    /// Ends the song one position later.
    EndLater(usize),
    /// Switches the song's fade-out.
    ToggleFadeOut(usize),
    /// Chooses the item's transition.
    Transition(usize, Transition),
    /// Slows the click-only tile by 1 BPM.
    BpmDown(usize),
    /// Speeds the click-only tile up by 1 BPM.
    BpmUp(usize),
    /// Chooses the click-only tile's meter, by its place in [`METERS`].
    Meter(usize, usize),
    /// One bar fewer (from one bar: until the operator moves on).
    BarsDown(usize),
    /// One bar more (from until the operator moves on: one bar).
    BarsUp(usize),
    /// Types into the song search.
    Search,
    /// Hides the last problem and the last warning.
    DismissNotice,
}

impl LibraryAction {
    /// Whether it is one of the remove controls, the only actions that
    /// leave a remove control armed ([`crate::action::disarms_removal`]).
    pub fn is_removal(self) -> bool {
        matches!(self, Self::RemoveSetlist | Self::RemoveUnreadable(_))
    }
}

/// What is being dragged: an item of the editor or a setlist of the list,
/// by its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dragged {
    /// An item of the setlist in the editor.
    Item(usize),
    /// A setlist of the list.
    Setlist(usize),
}

/// Which field the keys type into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The song search.
    Search,
    /// The name of the setlist in the editor.
    Rename,
}

/// The Library screen's state.
#[derive(Debug)]
pub struct LibraryPage {
    root: PathBuf,
    shelf: Shelf,
    catalog: Catalog,
    unreadable_songs: Vec<String>,
    editor: Option<SetlistEditor>,
    row: Option<usize>,
    search: LineEdit,
    rename: LineEdit,
    typing: Option<Field>,
    notice: Option<String>,
    warning: Option<String>,
    /// The editor's file moves not yet taken ([`Self::take_moves`]).
    moves: Vec<(String, String)>,
    /// The setlist file the remove control is armed for: the next press
    /// removes it.
    armed: Option<String>,
    /// The setlist file of this library open on Perform, which cannot be
    /// removed.
    playing: Option<String>,
    /// The drag under way and the gap it would land in, while dropping
    /// there would move it.
    insertion: Option<(Dragged, usize)>,
}

impl LibraryPage {
    /// The library folder `root`: its setlists, with `file` open in the
    /// editor when given (else the first one), and its songs.
    ///
    /// The folder is kept canonical ([`Self::root`]), so it compares with
    /// the library of a setlist on Perform as a plain path.
    pub fn open(root: &Path, file: Option<&str>) -> Result<Self, SetlistError> {
        Self::open_with_trash(root, file, library::move_to_trash)
    }

    /// The setlist in `from` was saved elsewhere (a move on the Perform
    /// strip) and is now in `to`: the list is read again, and an editor
    /// holding it reads it again, so the next edit starts from what was
    /// saved.
    pub fn setlist_saved(&mut self, from: &str, to: &str) {
        if let Err(error) = self.shelf.refresh() {
            self.notice = Some(error.to_string());
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.file() == from)
        {
            self.select(to);
        }
    }

    /// The setlist files the editor moved since the last call, as (from,
    /// to): a rename, or a file named by hand moved on its first change.
    pub fn take_moves(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.moves)
    }

    /// [`Self::open`], removing setlists with `trash` instead of the system
    /// trash ([`Library::with_trash`]).
    pub fn open_with_trash(
        root: &Path,
        file: Option<&str>,
        trash: TrashFn,
    ) -> Result<Self, SetlistError> {
        let root = &std::fs::canonicalize(root).map_err(|source| SetlistError::Folder {
            path: root.to_owned(),
            source,
        })?;
        let library = Library::new(root).with_trash(trash);
        let (catalog, unreadable_songs) = match library.list_songs() {
            Ok(listing) => (
                Catalog::from_listing(&listing),
                listing
                    .unreadable
                    .iter()
                    .map(|song| song.error.to_string())
                    .collect(),
            ),
            Err(error) => (Catalog::default(), vec![error.to_string()]),
        };
        let shelf = Shelf::open(library)?;
        let mut page = Self {
            root: root.to_owned(),
            shelf,
            catalog,
            unreadable_songs,
            editor: None,
            row: None,
            search: LineEdit::default(),
            rename: LineEdit::default(),
            typing: None,
            notice: None,
            warning: None,
            moves: Vec::new(),
            armed: None,
            playing: None,
            insertion: None,
        };
        let file = file
            .map(str::to_owned)
            .or_else(|| page.shelf.setlists.first().map(|s| s.entry.file.clone()));
        if let Some(file) = file {
            page.select(&file);
        }
        Ok(page)
    }

    /// A drag of `dragged` is over row `over`, in its `half`: the
    /// insertion line goes where it would land, or nowhere when dropping
    /// there changes nothing. Dragging disarms the remove controls.
    pub fn drag_over(&mut self, dragged: Dragged, over: usize, half: Half) {
        self.armed = None;
        let (from, len) = self.drag_source(dragged);
        let slot = drop_slot(over, half);
        self.insertion = drop_index(from, slot, len).map(|_| (dragged, slot));
    }

    /// A drag starts: no line shows until it is over a row, whatever an
    /// earlier drag that never dropped left behind. Dragging disarms the
    /// remove controls.
    pub fn start_drag(&mut self) {
        self.armed = None;
        self.insertion = None;
    }

    /// The drag of `dragged` was dropped on its list: it moves to where
    /// the insertion line shows ([`LibraryAction::DropItem`],
    /// [`LibraryAction::DropSetlist`]), or nowhere without a line.
    pub fn drop_dragged(&mut self, dragged: Dragged) {
        // Taken either way: a line worked out for another row never lasts.
        let Some((shown, slot)) = self.insertion.take() else {
            return;
        };
        if shown != dragged {
            return;
        }
        let (from, len) = self.drag_source(dragged);
        if let Some(to) = drop_index(from, slot, len) {
            self.act(match dragged {
                Dragged::Item(_) => LibraryAction::DropItem { from, to },
                Dragged::Setlist(_) => LibraryAction::DropSetlist { from, to },
            });
        }
    }

    /// The drag ended anywhere else, or left its list: nothing moves and
    /// the line goes. Whether a line was showing.
    pub fn cancel_drag(&mut self) -> bool {
        self.insertion.take().is_some()
    }

    /// The row `dragged` starts from and the length of its list.
    fn drag_source(&self, dragged: Dragged) -> (usize, usize) {
        match dragged {
            Dragged::Item(from) => (from, self.items().len()),
            Dragged::Setlist(from) => (from, self.shelf.setlists.len()),
        }
    }

    /// Disarms the remove controls: the Library screen was left, or
    /// something outside it happened.
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// Tells the page which setlist file of its library is open on the
    /// Perform screen, which cannot be removed.
    pub fn set_playing(&mut self, file: Option<&str>) {
        self.playing = file.map(str::to_owned);
    }

    /// The library folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file of the setlist in the editor.
    pub fn selected_file(&self) -> Option<&str> {
        self.editor.as_ref().map(SetlistEditor::file)
    }

    /// The setlist in the editor.
    pub fn editor(&self) -> Option<&SetlistEditor> {
        self.editor.as_ref()
    }

    /// The field the keys type into, if any.
    pub fn typing(&self) -> Option<Field> {
        self.typing
    }

    /// The last problem, until dismissed or replaced.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Carries out `action`; what goes wrong is kept as the notice.
    pub fn act(&mut self, action: LibraryAction) {
        if let Err(problem) = self.try_act(action) {
            self.notice = Some(problem);
        }
    }

    fn try_act(&mut self, action: LibraryAction) -> Result<(), String> {
        use LibraryAction as A;
        // Removing takes two presses in a row: anything else disarms it.
        if !action.is_removal() {
            self.armed = None;
        }
        match action {
            A::SelectSetlist(row) => {
                if let Some(file) = self.shelf.setlists.get(row).map(|s| s.entry.file.clone()) {
                    self.select(&file);
                }
            }
            A::NewSetlist => {
                let (entry, warning) = self.shelf.create().map_err(|e| e.to_string())?;
                self.warn(warning);
                self.select(&entry.file);
                self.start_rename();
            }
            A::RenameSetlist => self.start_rename(),
            A::MoveSetlistUp(index) if index > 0 => self
                .shelf
                .move_setlist(index, index - 1)
                .map_err(|e| e.to_string())?,
            A::MoveSetlistDown(index) if index + 1 < self.shelf.setlists.len() => self
                .shelf
                .move_setlist(index, index + 1)
                .map_err(|e| e.to_string())?,
            A::MoveSetlistUp(_) | A::MoveSetlistDown(_) => {}
            A::DropItem { from, to } => {
                let len = self.items().len();
                if from != to && from < len && to < len {
                    self.move_item(from, to)?;
                }
            }
            A::DropSetlist { from, to } => {
                let len = self.shelf.setlists.len();
                if from != to && from < len && to < len {
                    self.shelf
                        .move_setlist(from, to)
                        .map_err(|e| e.to_string())?;
                }
            }
            A::RemoveSetlist => self.remove_selected()?,
            A::RemoveUnreadable(index) => self.remove_unreadable(index)?,
            A::DuplicateSetlist => {
                if let Some(file) = self.selected_file().map(str::to_owned) {
                    let (entry, warning) =
                        self.shelf.duplicate(&file).map_err(|e| e.to_string())?;
                    self.warn(warning);
                    self.select(&entry.file);
                    self.start_rename();
                }
            }
            A::SelectRow(row) => {
                self.row = if self.row == Some(row) {
                    None
                } else {
                    Some(row)
                };
            }
            // With no setlist in the editor there is nothing to add to.
            A::AddSong(_) | A::AddClick if self.editor.is_none() => {}
            A::AddSong(row) => {
                if let Some(song) = self.found_songs().get(row).map(|s| s.song.clone()) {
                    self.edit(Edit::AddSong(song))?;
                    self.row = self.items().len().checked_sub(1);
                }
            }
            A::AddClick => {
                self.edit(Edit::AddClick)?;
                self.row = self.items().len().checked_sub(1);
            }
            A::Remove(index) => {
                self.edit(Edit::Remove(index))?;
                self.row = match self.row {
                    Some(row) if row == index => None,
                    Some(row) if row > index => Some(row - 1),
                    row => row,
                };
            }
            A::MoveUp(index) if index > 0 => self.move_item(index, index - 1)?,
            A::MoveDown(index) if index + 1 < self.items().len() => {
                self.move_item(index, index + 1)?;
            }
            A::MoveUp(_) | A::MoveDown(_) => {}
            A::Arrangement(index, arrangement) => {
                let id = self
                    .song_entry(index)
                    .and_then(|song| song.arrangements.get(arrangement))
                    .map(|arrangement| arrangement.id.clone());
                if let Some(id) = id {
                    self.edit(Edit::Arrangement(index, id))?;
                }
            }
            A::StartEarlier(index)
            | A::StartLater(index)
            | A::EndEarlier(index)
            | A::EndLater(index) => {
                if let Some(edit) = self.move_bound(action, index) {
                    self.edit(edit)?;
                }
            }
            A::ToggleFadeOut(index) => {
                if let Some(SetlistItem::Song(song)) = self.items().get(index) {
                    let fade_out = !song.fade_out;
                    self.edit(Edit::FadeOut(index, fade_out))?;
                }
            }
            A::Transition(index, transition) => self.edit(Edit::Transition(index, transition))?,
            A::BpmDown(index) => {
                self.change_click(index, |c| c.bpm = (c.bpm - 1.0).max(MIN_BPM))?
            }
            A::BpmUp(index) => self.change_click(index, |c| c.bpm = (c.bpm + 1.0).min(MAX_BPM))?,
            A::Meter(index, meter) => {
                if let Some(&(numerator, denominator)) = METERS.get(meter) {
                    self.change_click(index, |c| {
                        c.time_signature = TimeSignature {
                            numerator,
                            denominator,
                        }
                    })?;
                }
            }
            A::BarsDown(index) => self.change_click(index, |c| {
                c.bars = c
                    .bars
                    .and_then(|bars| bars.checked_sub(1))
                    .filter(|&b| b > 0);
            })?,
            A::BarsUp(index) => {
                self.change_click(index, |c| c.bars = Some(c.bars.map_or(1, |b| b + 1)))?;
            }
            A::Search => self.typing = Some(Field::Search),
            A::DismissNotice => {
                self.notice = None;
                self.warning = None;
            }
        }
        Ok(())
    }

    /// A key typed into the field that has the focus.
    pub fn type_key(&mut self, key: LineKey<'_>) {
        self.armed = None;
        match self.typing {
            Some(Field::Search) => match self.search.key(key) {
                LineOutcome::Editing => {}
                LineOutcome::Commit(_) => self.typing = None,
                LineOutcome::Cancel => {
                    self.search = LineEdit::default();
                    self.typing = None;
                }
            },
            Some(Field::Rename) => match self.rename.key(key) {
                LineOutcome::Editing => {}
                LineOutcome::Commit(name) => {
                    self.typing = None;
                    self.rename_to(&name);
                }
                LineOutcome::Cancel => self.typing = None,
            },
            None => {}
        }
    }

    /// Text pasted into the field that has the focus.
    pub fn paste(&mut self, text: &str) {
        self.armed = None;
        match self.typing {
            Some(Field::Search) => self.search.paste(text),
            Some(Field::Rename) => self.rename.paste(text),
            None => {}
        }
    }

    /// Leaves the field that has the focus: the search keeps what was
    /// typed, a rename is not made.
    pub fn stop_typing(&mut self) {
        self.typing = None;
    }

    fn select(&mut self, file: &str) {
        self.row = None;
        if self.typing == Some(Field::Rename) {
            self.typing = None;
        }
        match SetlistEditor::open(self.shelf.library().clone(), file) {
            Ok(editor) => self.editor = Some(editor),
            Err(error) => {
                self.editor = None;
                self.notice = Some(error.to_string());
            }
        }
    }

    /// The remove control: the first press arms it for the setlist in the
    /// editor, the second moves that setlist's file to the trash and puts
    /// the next setlist (or the one before, or none) in the editor. The
    /// setlist open on Perform is refused.
    fn remove_selected(&mut self) -> Result<(), String> {
        let Some(editor) = &self.editor else {
            return Ok(());
        };
        let (name, editing) = (editor.setlist().name.clone(), editor.file().to_owned());
        // The name as the folder lists the file: a rename on a disk that
        // stores names decomposed lists it otherwise than it was written,
        // and the setlist on Perform is known by its listed name.
        let file = self
            .shelf
            .library()
            .listed_setlist_file(&editing)
            .ok()
            .flatten()
            .unwrap_or_else(|| editing.clone());
        let on_perform = self
            .playing
            .as_deref()
            .is_some_and(|playing| playing == file || playing == editing);
        if on_perform {
            self.armed = None;
            return Err(format!(
                "\"{name}\" is open on Perform, so it cannot be removed: open another setlist or a song there first."
            ));
        }
        if self.armed.as_deref() != Some(editing.as_str()) {
            self.armed = Some(editing);
            return Ok(());
        }
        self.armed = None;
        let index = self.shelf.position(&file);
        let (_, warning) = self.shelf.remove(&file).map_err(|e| e.to_string())?;
        self.warn(warning);
        let next = index.and_then(|index| {
            let setlists = &self.shelf.setlists;
            setlists
                .get(index)
                .or_else(|| index.checked_sub(1).and_then(|before| setlists.get(before)))
                .map(|setlist| setlist.entry.file.clone())
        });
        match next {
            Some(next) => self.select(&next),
            None => {
                self.editor = None;
                self.row = None;
            }
        }
        Ok(())
    }

    /// The remove control of a setlist file that cannot be read: the first
    /// press arms it, the second moves the file to the trash.
    fn remove_unreadable(&mut self, index: usize) -> Result<(), String> {
        let Some(file) = self.shelf.unreadable.get(index).map(|p| p.file.clone()) else {
            return Ok(());
        };
        if self.playing.as_deref() == Some(file.as_str()) {
            self.armed = None;
            return Err(format!(
                "{file} is open on Perform, so it cannot be removed: open another setlist or a song there first."
            ));
        }
        if self.armed.as_deref() != Some(file.as_str()) {
            self.armed = Some(file);
            return Ok(());
        }
        self.armed = None;
        let (_, warning) = self.shelf.remove(&file).map_err(|e| e.to_string())?;
        self.warn(warning);
        Ok(())
    }

    fn start_rename(&mut self) {
        if let Some(editor) = &self.editor {
            self.rename = LineEdit::new(editor.setlist().name.clone());
            self.typing = Some(Field::Rename);
        }
    }

    fn rename_to(&mut self, name: &str) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if name == editor.setlist().name {
            return;
        }
        let from = editor.file().to_owned();
        let renamed = editor.rename(name);
        self.moved(from);
        match renamed {
            Ok(warning) => {
                self.warn(warning);
                if let Err(error) = self.shelf.refresh() {
                    self.notice = Some(error.to_string());
                }
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    fn items(&self) -> &[SetlistItem] {
        self.editor
            .as_ref()
            .map_or(&[], |editor| editor.setlist().items.as_slice())
    }

    /// Makes `edit` in the editor and lists the setlists again, for their
    /// item counts.
    fn edit(&mut self, edit: Edit) -> Result<(), String> {
        let Some(editor) = &mut self.editor else {
            return Ok(());
        };
        let from = editor.file().to_owned();
        let applied = editor.apply(edit);
        self.moved(from);
        self.warn(applied.map_err(|error| error.to_string())?);
        self.shelf.refresh().map_err(|error| error.to_string())
    }

    /// Records that the editor's setlist moved from the file `from`, when
    /// it did.
    fn moved(&mut self, from: String) {
        if let Some(to) = self.selected_file().map(str::to_owned)
            && to != from
        {
            self.moves.push((from, to));
        }
    }

    /// Shows `warning`, when there is one, until dismissed or replaced.
    fn warn(&mut self, warning: Option<String>) {
        if warning.is_some() {
            self.warning = warning;
        }
    }

    fn move_item(&mut self, from: usize, to: usize) -> Result<(), String> {
        self.edit(Edit::Move { from, to })?;
        // The row shown keeps its item: the moved one goes with it, the
        // ones it passed shift one place.
        self.row = self.row.map(|row| match row {
            row if row == from => to,
            row if from < to && row > from && row <= to => row - 1,
            row if to < from && row >= to && row < from => row + 1,
            row => row,
        });
        Ok(())
    }

    fn change_click(
        &mut self,
        index: usize,
        change: impl FnOnce(&mut ClickItem),
    ) -> Result<(), String> {
        let Some(SetlistItem::Click(click)) = self.items().get(index) else {
            return Ok(());
        };
        let mut click = *click;
        change(&mut click);
        self.edit(Edit::Click(index, click))
    }

    /// The library's entry for the song of item `index`.
    fn song_entry(&self, index: usize) -> Option<&SongEntry> {
        match self.items().get(index)? {
            SetlistItem::Song(song) => self.catalog.song(&song.song),
            SetlistItem::Click(_) => None,
        }
    }

    /// The edit that moves the start or end of song item `index` one
    /// position, keeping start before end inside its arrangement; `None`
    /// when it cannot move.
    fn move_bound(&self, action: LibraryAction, index: usize) -> Option<Edit> {
        let SetlistItem::Song(song) = self.items().get(index)? else {
            return None;
        };
        let bounds = bounds(song, self.song_entry(index)?)?;
        // A bound at the arrangement's edge is stored as none.
        let start = |position: usize| (position > 0).then_some(position);
        let end = |position: usize| (position < bounds.last).then_some(position);
        let (first, last) = match action {
            LibraryAction::StartEarlier(_) if bounds.start > 0 => {
                (start(bounds.start - 1), song.end_position)
            }
            LibraryAction::StartLater(_) if bounds.start < bounds.end => {
                (start(bounds.start + 1), song.end_position)
            }
            LibraryAction::EndEarlier(_) if bounds.end > bounds.start => {
                (song.start_position, end(bounds.end - 1))
            }
            LibraryAction::EndLater(_) if bounds.end < bounds.last => {
                (song.start_position, end(bounds.end + 1))
            }
            _ => return None,
        };
        Some(Edit::StartEnd(index, first, last))
    }

    /// The library's songs that match the search, in its order.
    fn found_songs(&self) -> Vec<&SongEntry> {
        let query = self.search.text().trim().to_lowercase();
        self.catalog
            .songs()
            .iter()
            .filter(|song| {
                query.is_empty()
                    || song.title.to_lowercase().contains(&query)
                    || song
                        .key
                        .as_ref()
                        .is_some_and(|key| key.to_lowercase() == query)
                    || song.bpm.to_string().starts_with(&query)
            })
            .collect()
    }

    /// What the screen shows; `playing` is the setlist file open on the
    /// Perform screen when it is in this library.
    pub fn view(&self, playing: Option<&str>) -> LibraryView {
        let selected = self.selected_file();
        let setlists = self
            .shelf
            .setlists
            .iter()
            .enumerate()
            .map(|(index, setlist)| SetlistRow {
                index,
                name: setlist.entry.name.clone(),
                meta: plural(setlist.items, "item"),
                selected: selected == Some(setlist.entry.file.as_str()),
                playing: playing == Some(setlist.entry.file.as_str()),
            })
            .collect();
        let found = self.found_songs();
        let total = self.catalog.songs().len();
        let song_summary = if found.len() == total {
            plural(total, "song")
        } else {
            format!("{} of {}", found.len(), plural(total, "song"))
        };
        let songs = found
            .into_iter()
            .enumerate()
            .map(|(index, song)| song_row(index, song))
            .collect();
        LibraryView {
            folder: self.root.display().to_string(),
            setlists,
            unreadable_setlists: self
                .shelf
                .unreadable
                .iter()
                .enumerate()
                .map(|(index, problem)| {
                    let armed = self.armed.as_deref() == Some(problem.file.as_str());
                    UnreadableRow {
                        index,
                        line: problem.line.clone(),
                        remove: if armed {
                            ARMED_LABEL.to_owned()
                        } else {
                            "Remove".to_owned()
                        },
                        remove_armed: armed,
                    }
                })
                .collect(),
            order_problem: self.shelf.order_problem.clone(),
            setlist_insertion: match self.insertion {
                Some((Dragged::Setlist(_), slot)) => Some(slot),
                _ => None,
            },
            entry_problems: self.shelf.entry_problems.clone(),
            editor: self
                .editor
                .as_ref()
                .map(|editor| self.editor_view(editor, playing)),
            songs,
            song_summary,
            unreadable_songs: self.unreadable_songs.clone(),
            search: self.search.text().to_owned(),
            searching: self.typing == Some(Field::Search),
            notice: self.notice.clone(),
            warning: self.warning.clone(),
        }
    }

    fn editor_view(&self, editor: &SetlistEditor, playing: Option<&str>) -> EditorView {
        let setlist = editor.setlist();
        let armed = self.armed.as_deref() == Some(editor.file());
        let rows: Vec<EditorRow> = setlist
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| self.editor_row(editor, index, item))
            .collect();
        let lengths: Vec<Option<u64>> = setlist
            .items
            .iter()
            .map(|item| self.seconds(item))
            .collect();
        let summary = summary(&lengths);
        EditorView {
            name: setlist.name.clone(),
            renaming: (self.typing == Some(Field::Rename)).then(|| self.rename.text().to_owned()),
            summary,
            rows,
            problems: editor
                .problems()
                .iter()
                .filter(|problem| item_of(problem).is_none())
                .map(ToString::to_string)
                .collect(),
            playing: playing == Some(editor.file()),
            on_perform: (playing == Some(editor.file())).then(|| ON_PERFORM.to_owned()),
            remove: if armed {
                ARMED_LABEL.to_owned()
            } else {
                "Remove".to_owned()
            },
            remove_armed: armed,
            insertion: match self.insertion {
                Some((Dragged::Item(_), slot)) => Some(slot),
                _ => None,
            },
        }
    }

    fn editor_row(&self, editor: &SetlistEditor, index: usize, item: &SetlistItem) -> EditorRow {
        let selected = self.row == Some(index);
        let (arrangement, details) = match item {
            SetlistItem::Song(song) => {
                let entry = self.catalog.song(&song.song);
                let arrangement = entry
                    .and_then(|entry| entry.arrangements.iter().find(|a| a.id == song.arrangement))
                    .map_or_else(|| song.arrangement.clone(), |a| a.name.clone());
                let details = selected.then(|| RowDetails::Song(song_details(song, entry)));
                (arrangement, details)
            }
            SetlistItem::Click(click) => (
                "—".to_owned(),
                selected.then(|| RowDetails::Click(click_details(click))),
            ),
        };
        EditorRow {
            index,
            number: (index + 1).to_string(),
            title: self.catalog.title(item),
            // The arrangement has a column of its own in the editor.
            meta: self.catalog.meta_without_arrangement(item),
            click: matches!(item, SetlistItem::Click(_)),
            arrangement,
            transition: item.transition(),
            length: self.seconds(item).map_or_else(|| "—".to_owned(), clock),
            problems: editor
                .problems()
                .iter()
                .filter(|problem| item_of(problem) == Some(index))
                .map(ToString::to_string)
                .collect(),
            selected,
            details,
        }
    }

    /// How long `item` plays, in whole seconds; `None` when that depends on
    /// the operator or the library lacks its song.
    fn seconds(&self, item: &SetlistItem) -> Option<u64> {
        match item {
            SetlistItem::Song(song) => {
                let entry = self.catalog.song(&song.song)?;
                let arrangement = entry
                    .arrangements
                    .iter()
                    .find(|a| a.id == song.arrangement)?;
                let bounds = bounds(song, entry)?;
                let frames = arrangement
                    .positions
                    .get(bounds.start..=bounds.end)?
                    .iter()
                    .fold(0, |total: u64, p| total.saturating_add(p.length_frames));
                Some(frames / u64::from(entry.sample_rate.max(1)))
            }
            // A tempo the setlist rules refuse has no length; the domain's
            // problem shows instead.
            SetlistItem::Click(click) if !(MIN_BPM..=MAX_BPM).contains(&click.bpm) => None,
            SetlistItem::Click(click) => {
                let beats = f64::from(click.bars?) * f64::from(click.time_signature.numerator);
                // At most u32::MAX² beats at 1 BPM: finite, and `as`
                // saturates whatever is left over.
                Some((beats * 60.0 / click.bpm) as u64)
            }
        }
    }
}

/// Where a song item starts and ends, counted from 0, and its
/// arrangement's last position.
struct Bounds {
    start: usize,
    end: usize,
    last: usize,
}

/// The bounds of `song` in `entry`; `None` when the song has no such
/// arrangement or it is empty.
fn bounds(song: &SongItem, entry: &SongEntry) -> Option<Bounds> {
    let positions = entry
        .arrangements
        .iter()
        .find(|a| a.id == song.arrangement)?
        .positions
        .len();
    let last = positions.checked_sub(1)?;
    Some(Bounds {
        start: song.start_position.unwrap_or(0),
        end: song.end_position.unwrap_or(last),
        last,
    })
}

fn song_details(song: &SongItem, entry: Option<&SongEntry>) -> SongDetails {
    let arrangements = entry.map_or_else(Vec::new, |entry| {
        entry
            .arrangements
            .iter()
            .map(|arrangement| Choice {
                label: arrangement.name.clone(),
                selected: arrangement.id == song.arrangement,
            })
            .collect()
    });
    let arrangement =
        entry.and_then(|entry| entry.arrangements.iter().find(|a| a.id == song.arrangement));
    let bounds = entry.and_then(|entry| bounds(song, entry));
    let label = |position: Option<usize>, edge: &str| match (position, arrangement) {
        (Some(position), Some(arrangement)) => arrangement.positions.get(position).map_or_else(
            || format!("{} · not in the arrangement", position.saturating_add(1)),
            |p| format!("{} · {}", position + 1, p.label),
        ),
        (Some(position), None) => format!("position {}", position.saturating_add(1)),
        (None, _) => edge.to_owned(),
    };
    let start = bounds.as_ref().map(|b| b.start).or(song.start_position);
    let end = bounds.as_ref().map(|b| b.end).or(song.end_position);
    SongDetails {
        arrangements,
        start: label(start, "the first position"),
        end: label(end, "the last position"),
        start_earlier: bounds.as_ref().is_some_and(|b| b.start > 0),
        start_later: bounds.as_ref().is_some_and(|b| b.start < b.end),
        end_earlier: bounds.as_ref().is_some_and(|b| b.end > b.start),
        end_later: bounds.as_ref().is_some_and(|b| b.end < b.last),
        fade_out: song.fade_out,
    }
}

fn click_details(click: &ClickItem) -> ClickDetails {
    let meter = click.time_signature;
    ClickDetails {
        bpm: format!("{} BPM", click.bpm),
        meters: METERS
            .iter()
            .map(|&(numerator, denominator)| Choice {
                label: format!("{numerator}/{denominator}"),
                selected: meter.numerator == numerator && meter.denominator == denominator,
            })
            .collect(),
        bars: match click.bars {
            Some(bars) => plural(bars as usize, "bar"),
            None => "Until you move on".to_owned(),
        },
    }
}

fn song_row(index: usize, song: &SongEntry) -> SongRow {
    let default = song
        .arrangements
        .iter()
        .find(|a| a.id == DEFAULT_ARRANGEMENT_ID)
        .or_else(|| song.arrangements.first());
    let frames = default.map_or(song.length_frames, ArrangementEntry::length_frames);
    SongRow {
        index,
        title: song.title.clone(),
        path: song.song.clone(),
        key: song.key.clone().unwrap_or_else(|| "—".to_owned()),
        bpm: song.bpm.to_string(),
        length: clock(frames / u64::from(song.sample_rate.max(1))),
        stems: plural(song.stems, "stem"),
    }
}

/// The editor header's summary of items lasting `lengths` (`None` where
/// that depends on the operator or the library lacks the song), as the
/// design writes it: "5 items · ≈ 23 min". With a length left open, the
/// known part is a floor: "5 items · at least 22 min", and nothing about
/// the length under a minute.
fn summary(lengths: &[Option<u64>]) -> String {
    if lengths.is_empty() {
        return "No items yet".to_owned();
    }
    let items = plural(lengths.len(), "item");
    let total = lengths
        .iter()
        .flatten()
        .fold(0, |total: u64, seconds| total.saturating_add(*seconds));
    if lengths.iter().any(Option::is_none) {
        return match total / 60 {
            0 => items,
            minutes => format!("{items} · at least {minutes} min"),
        };
    }
    // Rounded to the nearest minute, half a minute up.
    match total.saturating_add(30) / 60 {
        0 => format!("{items} · < 1 min"),
        minutes => format!("{items} · ≈ {minutes} min"),
    }
}

/// "1 item", "5 items".
fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// `seconds` as "m:ss".
fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The item a validation problem is about; `None` for the setlist as a
/// whole.
fn item_of(problem: &SetlistValidationError) -> Option<usize> {
    use SetlistValidationError as E;
    match problem {
        E::UnsupportedFormatVersion { .. } | E::EmptyName => None,
        E::InvalidSongPath { item, .. }
        | E::UnknownSong { item, .. }
        | E::UnknownArrangement { item, .. }
        | E::StartAfterEnd { item, .. }
        | E::PositionOutsideArrangement { item, .. }
        | E::InvalidClickBpm { item, .. }
        | E::InvalidClickNumerator { item }
        | E::InvalidClickDenominator { item, .. }
        | E::ZeroClickBars { item } => Some(*item),
    }
}

/// The library folder (and setlist) the Library screen opens on when it
/// is shown: the library of the setlist `playing` on Perform, with that
/// setlist in the editor, when no library is `open` yet or the open one was
/// followed from another setlist; a library picked with Open library…
/// stays. Libraries are compared as stored: both canonical.
pub fn library_to_open(
    open: Option<(&Path, Opened)>,
    playing: Option<(&Path, &str)>,
) -> Option<(PathBuf, String)> {
    let (library, file) = playing?;
    match open {
        Some((_, Opened::Picked)) => None,
        Some((root, Opened::Followed)) if root == library => None,
        _ => Some((library.to_owned(), file.to_owned())),
    }
}

/// How the library the Library screen shows was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// Picked with Open library…: it stays until another is picked.
    Picked,
    /// Followed from the setlist on Perform: it follows the one playing.
    Followed,
}

/// A setlist file of the list that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableRow {
    /// Its row; removes it.
    pub index: usize,
    /// The file and why it cannot be read.
    pub line: String,
    /// The remove control: "Remove", or [`ARMED_LABEL`] once armed.
    pub remove: String,
    /// Whether the next press removes it.
    pub remove_armed: bool,
}

/// One setlist of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetlistRow {
    /// Its row; selects it.
    pub index: usize,
    /// Its name.
    pub name: String,
    /// How many items, such as "5 items".
    pub meta: String,
    /// Whether it is in the editor.
    pub selected: bool,
    /// Whether it is open on the Perform screen.
    pub playing: bool,
}

/// One of several choices, such as an arrangement or a meter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What it reads.
    pub label: String,
    /// Whether it is the one chosen.
    pub selected: bool,
}

/// The controls of a song item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongDetails {
    /// The song's arrangements, the item's one selected; empty when the
    /// library does not have the song.
    pub arrangements: Vec<Choice>,
    /// Where it starts, such as "1 · Intro".
    pub start: String,
    /// Where it ends, such as "4 · Outro".
    pub end: String,
    /// Whether it can start earlier.
    pub start_earlier: bool,
    /// Whether it can start later.
    pub start_later: bool,
    /// Whether it can end earlier.
    pub end_earlier: bool,
    /// Whether it can end later.
    pub end_later: bool,
    /// Whether it fades out over its last five seconds.
    pub fade_out: bool,
}

/// The controls of a click-only tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickDetails {
    /// Its tempo, such as "120 BPM".
    pub bpm: String,
    /// The meters of [`METERS`], its own selected.
    pub meters: Vec<Choice>,
    /// How long it plays, such as "8 bars" or "Until you move on".
    pub bars: String,
}

/// The controls of the selected item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowDetails {
    /// A song.
    Song(SongDetails),
    /// A click-only tile.
    Click(ClickDetails),
}

/// One item of the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRow {
    /// Index of the item.
    pub index: usize,
    /// Its number, counted from 1.
    pub number: String,
    /// Song title or "Click".
    pub title: String,
    /// What it plays.
    pub meta: String,
    /// Whether it is a click-only tile.
    pub click: bool,
    /// Its arrangement's name, or "—" for a click-only tile.
    pub arrangement: String,
    /// Its transition.
    pub transition: Transition,
    /// How long it plays, such as "4:48", or "—" when that depends on the
    /// operator or the library lacks the song.
    pub length: String,
    /// The rules it breaks, as the `domain` crate words them.
    pub problems: Vec<String>,
    /// Whether its details show.
    pub selected: bool,
    /// Its controls, when selected.
    pub details: Option<RowDetails>,
}

/// The setlist editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorView {
    /// The setlist's name.
    pub name: String,
    /// The name being typed, while renaming.
    pub renaming: Option<String>,
    /// How many items and about how long, such as "5 items · ≈ 23 min".
    pub summary: String,
    /// The items.
    pub rows: Vec<EditorRow>,
    /// Rules the setlist as a whole breaks.
    pub problems: Vec<String>,
    /// Whether it is open on the Perform screen, which plays it as it was
    /// when opened.
    pub playing: bool,
    /// The line above the name while it is open on the Perform screen:
    /// [`ON_PERFORM`].
    pub on_perform: Option<String>,
    /// The remove control: "Remove", or [`ARMED_LABEL`] once armed.
    pub remove: String,
    /// Whether the next press of the remove control removes the setlist.
    pub remove_armed: bool,
    /// Where a dragged item would land: before the row at this index, or
    /// after the last row when it equals the row count.
    pub insertion: Option<usize>,
}

/// One song of the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongRow {
    /// Its row; adds it to the setlist.
    pub index: usize,
    /// Its title.
    pub title: String,
    /// Its folder, relative to the library folder.
    pub path: String,
    /// Key, or "—".
    pub key: String,
    /// Tempo, such as "132".
    pub bpm: String,
    /// Length of its Default arrangement, such as "4:48".
    pub length: String,
    /// Stem count, such as "14 stems".
    pub stems: String,
}

/// Everything the Library screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryView {
    /// The library folder.
    pub folder: String,
    /// The setlists.
    pub setlists: Vec<SetlistRow>,
    /// Setlist files that cannot be read, with the reason.
    pub unreadable_setlists: Vec<UnreadableRow>,
    /// Why the stored order of the setlists cannot be used, so they are
    /// listed by name.
    pub order_problem: Option<String>,
    /// Where a dragged setlist would land in the list, as
    /// [`EditorView::insertion`].
    pub setlist_insertion: Option<usize>,
    /// Entries of the setlists folder that could not be read at all; every
    /// other setlist is listed.
    pub entry_problems: Vec<String>,
    /// The editor, when a setlist is selected.
    pub editor: Option<EditorView>,
    /// The songs that match the search, by title.
    pub songs: Vec<SongRow>,
    /// How many songs, such as "12 songs" or "3 of 12 songs".
    pub song_summary: String,
    /// Song folders that cannot be read, with the reason.
    pub unreadable_songs: Vec<String>,
    /// The search text.
    pub search: String,
    /// Whether the keys type into the search.
    pub searching: bool,
    /// The last problem.
    pub notice: Option<String>,
    /// The last warning: something done that is not quite whole, such as a
    /// setlist saved without its place in the stored order.
    pub warning: Option<String>,
}
