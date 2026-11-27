//! The Song editor for a song already in the library (S4, F2) without a
//! window. The Library's edit link reads the song off the UI thread
//! ([`OpenSongJob`]): its `song.json`, its length, and the setlists that
//! play each of its arrangements. [`SongEdit`] then edits it as the design
//! canvas's Song editor does: the header and the stems as for an import
//! ([`crate::song_header`], [`ImportAction`]), and its section markers and
//! arrangements ([`SongAction`]) through the `domain` crate's
//! [`SongEditor`], whose every refusal shows as the editor's note and
//! changes nothing. [`SongEdit::view`] says what it shows
//! ([`SongEditView`]).
//!
//! The waveform is read off the UI thread too ([`WaveformJob`]): the
//! song's cached overview, or one built from its stems, with its progress;
//! closing the editor cancels the build. Its bars follow the bar grid, so
//! a tempo or meter edit redraws them from the same overview.
//!
//! "Save song" rewrites `song.json` off the UI thread ([`SaveSongJob`],
//! [`library::Library::save_song`]); an arrangement deleted also leaves
//! the song's `mix.json`. Leaving with edits not saved asks first
//! ([`crate::ending::Leaving`]).

mod jobs;
mod view;

use std::collections::BTreeMap;

use domain::{
    BarMaps, DEFAULT_ARRANGEMENT_ID, SectionType, Snap, SongEditError, SongEditor, SongPackage,
};
use library::Library;

use crate::line_edit::{LineEdit, LineKey, LineOutcome};
use crate::reorder::{Half, drop_index, drop_slot};
use crate::song_header::{HeaderFields, HeaderValues};
use crate::song_import::{ImportAction, SongField, full_stop, next_ticket, sentence};
use crate::song_sections::{BarScale, MIN_VISIBLE_BARS, Window};

pub(crate) use jobs::SongSource;
pub(crate) use jobs::WaveformRun;
pub use jobs::{OpenSongJob, OpenedSong, SaveSongJob, SongSaved, Waved, WaveformJob};
use jobs::{Waveform, package_problems};
pub use view::stems_summary;

/// The name "New" gives an arrangement, numbered when taken: "New
/// arrangement 2".
pub const NEW_ARRANGEMENT_NAME: &str = "New arrangement";

/// The line under "Infinite loop by default", as on the design canvas.
pub const LOOP_HINT: &str = "Repeats until you switch it off live";

/// The note of a song whose tempo or meter changes do not start bars.
pub const READ_ONLY_MAPS: &str = "This song's tempo or meter changes do not start bars, so Tempo, Time and its changes are read-only here; they are saved as they are.";

/// A control of the Song editor's sections and arrangements; indexes are
/// markers, arrangements and positions as [`SongEditView`] shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongAction {
    /// A block of the marker strip: the inspector shows its section.
    SelectSection(usize),
    /// A click on the waveform or the ruler, this far across the bars
    /// shown (out of [`crate::song_sections::ALONG`]): the cursor goes there, snapped.
    PlaceCursor(u32),
    /// "Add marker at playhead": a marker at the cursor, splitting the
    /// section it falls in.
    AddMarker,
    /// "Snap to bar": on, markers snap to bars; off, to beats.
    ToggleSnap,
    /// Shows fewer bars, around the cursor.
    ZoomIn,
    /// Shows more bars.
    ZoomOut,
    /// Scrolls through the song by this many thousandths of a bar.
    Scroll(i32),
    /// Drops marker `index` this far across the bars shown (out of
    /// [`crate::song_sections::ALONG`]), snapped: a drag released ([`SongEdit::drop_marker`]).
    MoveMarker {
        /// The marker dragged.
        index: usize,
        /// Where it was released.
        along: u32,
    },
    /// Opens or closes the inspector's type menu.
    ToggleTypeMenu,
    /// Gives the section the type at this index of
    /// [`domain::SectionType::ALL`], chosen in the menu, which closes.
    SetType(usize),
    /// Starts typing the section's number.
    EditNumber,
    /// "Infinite loop by default", for the arrangement shown.
    ToggleLoop,
    /// "Delete marker".
    DeleteMarker,
    /// Shows the arrangement of this tab.
    SelectArrangement(usize),
    /// "New": an arrangement playing every section once, named and then
    /// renamed in place.
    NewArrangement,
    /// Starts renaming the arrangement shown.
    RenameArrangement,
    /// Deletes the arrangement shown.
    DeleteArrangement,
    /// The × of this chip: its position leaves the arrangement.
    RemoveChip(usize),
    /// Adds this section (a marker) at the end of the arrangement.
    AppendSection(usize),
    /// Moves the chip at `from` to `to`: a drag dropped
    /// ([`SongEdit::drop_chip`]).
    DropChip {
        /// The chip dragged.
        from: usize,
        /// The index it ends up at.
        to: usize,
    },
}

/// What a Song editor control asks of the Library screen.
#[derive(Debug)]
pub(crate) enum EditOutcome {
    /// The editor stays.
    Stay,
    /// The editor closes: the Library shows again.
    Close,
    /// Leaving would lose edits: ask first.
    Ask,
    /// This save is to run off the UI thread.
    Save(Box<SaveSongJob>),
}

/// A local field of the editor that the keys type into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// The inspector's number.
    Number,
    /// The name of the arrangement shown.
    Rename,
}

/// A song of the library open in the Song editor.
#[derive(Debug)]
pub struct SongEdit {
    song: String,
    /// The package as opened: leaving asks first once the edit differs.
    original: SongPackage,
    editor: SongEditor,
    header: HeaderFields,
    values: HeaderValues,
    maps_read_only: bool,
    /// What the tempo and meter fields break, as sentences.
    map_problems: Vec<String>,
    menu: Option<usize>,
    all_stems: bool,
    selected: usize,
    arrangement: String,
    cursor: u64,
    snap: Snap,
    /// How many bars show; `None`: the whole song.
    visible: Option<u64>,
    /// The first bar shown, in thousandths of a bar.
    scroll: u64,
    /// The marker dragged and the frame under the pointer.
    marker_drag: Option<(usize, u64)>,
    /// The chip dragged and the gap it would land in.
    chip_insertion: Option<(usize, usize)>,
    type_menu: bool,
    number: LineEdit,
    rename: LineEdit,
    typing: Option<Typing>,
    /// The text of the local field typed into, as it was when typing
    /// started: Escape puts it back.
    before: String,
    in_use: BTreeMap<String, Vec<String>>,
    /// The last refusal or failed save, until the next edit that applies.
    note: Option<String>,
    waveform: Waveform,
    pending_waveform: Option<WaveformJob>,
    /// The ticket of the save under way.
    saving: Option<u64>,
}

impl SongEdit {
    /// The editor for `song` of `library`, as `source` read it; the
    /// package's broken rules when it cannot be edited.
    pub(crate) fn open(library: &Library, song: &str, source: SongSource) -> Result<Self, String> {
        let SongSource {
            package,
            length,
            in_use,
        } = source;
        let editor =
            SongEditor::new(package.clone(), length).map_err(|errors| package_problems(&errors))?;
        let maps = BarMaps::from_maps(
            package.sample_rate,
            length,
            &package.tempo_map,
            &package.time_signature_map,
        );
        let maps_read_only = maps.is_err();
        let maps = maps.unwrap_or_else(|_| BarMaps {
            bpm: package.tempo_map.first().map_or(0.0, |point| point.bpm),
            time_signature: package.time_signature_map.first().map_or(
                domain::DEFAULT_IMPORT_TIME_SIGNATURE,
                |point| domain::TimeSignature {
                    numerator: point.numerator,
                    denominator: point.denominator,
                },
            ),
            changes: Vec::new(),
        });
        let values = HeaderValues {
            title: package.title.clone(),
            key: package.key.clone(),
            bpm: maps.bpm,
            time_signature: maps.time_signature,
            changes: maps.changes,
        };
        let (job, run) = WaveformRun::start(library.root().join(song));
        let arrangement = package
            .arrangements
            .iter()
            .find(|arrangement| arrangement.id == DEFAULT_ARRANGEMENT_ID)
            .or_else(|| package.arrangements.first())
            .map(|arrangement| arrangement.id.clone())
            .unwrap_or_default();
        let mut edit = Self {
            song: song.to_owned(),
            header: HeaderFields::new(&values),
            values,
            maps_read_only,
            map_problems: Vec::new(),
            menu: None,
            all_stems: false,
            selected: 0,
            arrangement,
            cursor: 0,
            snap: Snap::Bar,
            visible: None,
            scroll: 0,
            marker_drag: None,
            chip_insertion: None,
            type_menu: false,
            number: LineEdit::default(),
            rename: LineEdit::default(),
            typing: None,
            before: String::new(),
            in_use,
            note: None,
            waveform: Waveform::Reading(run),
            pending_waveform: Some(job),
            saving: None,
            original: package,
            editor,
        };
        edit.reset_number();
        Ok(edit)
    }

    /// The waveform job the editor started, to run off the UI thread, once.
    pub(crate) fn take_waveform(&mut self) -> Option<WaveformJob> {
        self.pending_waveform.take()
    }

    /// What the waveform job gave; a job of another editor changes
    /// nothing.
    pub(crate) fn waveform_done(&mut self, done: Waved) {
        let ours = matches!(&self.waveform, Waveform::Reading(run) if run.ticket == done.ticket);
        if !ours {
            return;
        }
        self.waveform = match done.result {
            Ok(overview) => Waveform::Ready(overview),
            Err(error) => Waveform::Failed(full_stop(format!(
                "The waveform could not be drawn: {error}"
            ))),
        };
    }

    /// Whether the waveform job's progress changed since it was last drawn,
    /// which then counts as drawn.
    pub(crate) fn waveform_moved(&mut self) -> bool {
        let Waveform::Reading(run) = &mut self.waveform else {
            return false;
        };
        let percent = run.percent();
        let moved = percent != run.drawn;
        run.drawn = percent;
        moved
    }

    /// Whether the edit differs from the song as opened.
    pub(crate) fn has_edits(&self) -> bool {
        self.package() != self.original
    }

    /// Whether a save is under way.
    pub(crate) fn saving(&self) -> Option<u64> {
        self.saving
    }

    /// Whether the keys type into a field.
    pub(crate) fn takes_keys(&self) -> bool {
        self.header.typing().is_some() || self.typing.is_some()
    }

    /// The package as edited: the editor's, with the header's title and
    /// key.
    fn package(&self) -> SongPackage {
        let mut package = self.editor.package().clone();
        let title = self.values.title.trim();
        if title != self.original.title.trim() {
            package.title = title.to_owned();
        }
        let key = self.values.key.as_deref().map(str::trim);
        if key != self.original.key.as_deref().map(str::trim) {
            package.key = key.map(str::to_owned);
        }
        package
    }

    /// Carries out a header or stems control, as the import's editor has
    /// them.
    pub(crate) fn act_header(&mut self, action: ImportAction, library: &Library) -> EditOutcome {
        if self.saving.is_some() {
            return EditOutcome::Stay;
        }
        if !matches!(action, ImportAction::Edit(_)) {
            self.header.stop_typing();
        }
        if !matches!(action, ImportAction::TogglePartMenu(_)) {
            self.menu = None;
        }
        self.stop_local_typing();
        self.type_menu = false;
        let maps_field = |field: SongField| !matches!(field, SongField::Title | SongField::Key);
        match action {
            ImportAction::Edit(field) if self.maps_read_only && maps_field(field) => {}
            ImportAction::Edit(field) => self.header.start_typing(field),
            ImportAction::AddChange if self.maps_read_only => {}
            ImportAction::AddChange => {
                self.header.add_change(&self.values);
                self.sync_header();
            }
            ImportAction::RemoveChange(index) => {
                if self.header.remove_change(index, &mut self.values) {
                    self.sync_header();
                }
            }
            ImportAction::TogglePartMenu(index) => {
                self.menu = match self.menu {
                    Some(open) if open == index => None,
                    _ if index < self.editor.package().stems.len() => Some(index),
                    open => open,
                };
            }
            ImportAction::ClosePartMenu => {}
            ImportAction::SetPart(index, part) => {
                if index < self.editor.package().stems.len() {
                    self.rebuild(|package| {
                        let stem = &mut package.stems[index];
                        stem.part = part.id().to_owned();
                        stem.role = part.role();
                    });
                }
            }
            ImportAction::ShowAllStems => self.all_stems = true,
            ImportAction::Save => {
                if self.problems().is_empty() {
                    return EditOutcome::Save(Box::new(self.start_save(library)));
                }
            }
            ImportAction::Discard if self.has_edits() => return EditOutcome::Ask,
            ImportAction::Discard => return EditOutcome::Close,
        }
        EditOutcome::Stay
    }

    /// Carries out a sections or arrangements control.
    pub(crate) fn act(&mut self, action: SongAction) {
        if self.saving.is_some() {
            return;
        }
        // A click on the field typed into keeps typing; anything else
        // leaves it.
        let keeps = matches!(
            (action, self.typing),
            (SongAction::EditNumber, Some(Typing::Number))
                | (SongAction::RenameArrangement, Some(Typing::Rename))
        );
        if !keeps {
            self.stop_local_typing();
        }
        self.header.stop_typing();
        self.menu = None;
        if action != SongAction::ToggleTypeMenu {
            self.type_menu = false;
        }
        match action {
            SongAction::SelectSection(index) => self.select(index),
            SongAction::PlaceCursor(along) => {
                let at = self.frames_at(along);
                self.cursor = self.editor.snap(at, self.snap);
            }
            SongAction::AddMarker => {
                let added = self.editor.add_marker(self.cursor, self.snap);
                if let Some(id) = self.applied(added)
                    && let Some(index) = self.marker_index(&id)
                {
                    self.select(index);
                }
            }
            SongAction::ToggleSnap => {
                self.snap = match self.snap {
                    Snap::Bar => Snap::Beat,
                    Snap::Beat => Snap::Bar,
                }
            }
            SongAction::ZoomIn => self.zoom(true),
            SongAction::ZoomOut => self.zoom(false),
            SongAction::Scroll(thousandths) => {
                let room = self.total_bars().saturating_sub(self.visible_bars()) * 1000;
                self.scroll = self
                    .scroll
                    .saturating_add_signed(i64::from(thousandths))
                    .min(room);
            }
            SongAction::MoveMarker { index, along } => {
                self.marker_drag = None;
                let at = self.frames_at(along);
                self.move_marker(index, at);
            }
            SongAction::ToggleTypeMenu => self.type_menu = !self.type_menu,
            SongAction::SetType(choice) => {
                if let (Some(&section_type), Some(marker)) =
                    (SectionType::ALL.get(choice), self.selected_marker())
                {
                    let (id, number) = (marker.id.clone(), marker.number);
                    let set = self.editor.set_marker_type(&id, section_type, number);
                    self.applied(set);
                }
            }
            SongAction::EditNumber => {
                if self.typing != Some(Typing::Number) {
                    self.reset_number();
                    self.before = self.number.text().to_owned();
                    self.typing = Some(Typing::Number);
                }
            }
            SongAction::ToggleLoop => {
                if let Some(marker) = self.selected_marker() {
                    let id = marker.id.clone();
                    let on = self.looped(&id);
                    let set = self.editor.set_infinite_loop(&self.arrangement, &id, !on);
                    self.applied(set);
                }
            }
            SongAction::DeleteMarker => {
                if let Some(marker) = self.selected_marker() {
                    let id = marker.id.clone();
                    let deleted = self.editor.delete_marker(&id);
                    if self.applied(deleted).is_some() {
                        self.select(self.selected.saturating_sub(1));
                    }
                }
            }
            SongAction::SelectArrangement(index) => {
                if let Some(arrangement) = self.editor.package().arrangements.get(index) {
                    self.arrangement = arrangement.id.clone();
                    self.chip_insertion = None;
                }
            }
            SongAction::NewArrangement => self.new_arrangement(),
            SongAction::RenameArrangement => self.start_rename(),
            SongAction::DeleteArrangement => self.delete_arrangement(),
            SongAction::RemoveChip(index) => {
                let removed = self.editor.remove_section(&self.arrangement, index);
                self.applied(removed);
            }
            SongAction::AppendSection(index) => {
                if let Some(id) = self
                    .editor
                    .package()
                    .sections
                    .get(index)
                    .map(|m| m.id.clone())
                {
                    let added = self.editor.append_section(&self.arrangement, &id);
                    self.applied(added);
                }
            }
            SongAction::DropChip { from, to } => {
                self.chip_insertion = None;
                let moved = self.editor.move_section(&self.arrangement, from, to);
                self.applied(moved);
            }
        }
    }

    /// A key typed into the field that has the focus.
    pub(crate) fn type_key(&mut self, key: LineKey<'_>) {
        if self.header.typing().is_some() {
            if self.header.type_key(key) {
                self.sync_header();
            }
            return;
        }
        match self.typing {
            Some(Typing::Number) => match self.number.key(key) {
                LineOutcome::Editing => self.apply_number(),
                LineOutcome::Commit(_) => self.typing = None,
                LineOutcome::Cancel => {
                    self.number = LineEdit::new(std::mem::take(&mut self.before));
                    self.typing = None;
                    self.apply_number();
                }
            },
            Some(Typing::Rename) => match self.rename.key(key) {
                LineOutcome::Editing => {}
                LineOutcome::Commit(name) => {
                    self.typing = None;
                    let id = self.arrangement.clone();
                    let unchanged = self
                        .editor
                        .package()
                        .arrangements
                        .iter()
                        .any(|arrangement| arrangement.id == id && arrangement.name == name);
                    if !unchanged {
                        let renamed = self.editor.rename_arrangement(&id, &name);
                        self.applied(renamed);
                    }
                }
                LineOutcome::Cancel => self.typing = None,
            },
            None => {}
        }
    }

    /// Text pasted into the field that has the focus.
    pub(crate) fn paste(&mut self, text: &str) {
        if self.header.paste(text) {
            self.sync_header();
            return;
        }
        match self.typing {
            Some(Typing::Number) => {
                self.number.paste(text);
                self.apply_number();
            }
            Some(Typing::Rename) => self.rename.paste(text),
            None => {}
        }
    }

    /// Leaves the field that has the focus: a header field and the number
    /// keep their text, a rename is not made.
    pub(crate) fn stop_typing(&mut self) {
        self.header.stop_typing();
        self.stop_local_typing();
    }

    /// A drag of marker `index` is this far across the bars shown (out of
    /// [`crate::song_sections::ALONG`]): the place it would land shows, snapped.
    pub(crate) fn drag_marker(&mut self, index: usize, along: u32) {
        if index > 0 && index < self.editor.package().sections.len() {
            self.marker_drag = Some((index, self.frames_at(along)));
        }
    }

    /// The drag of marker `index` was released over the bars: it moves
    /// where its ghost shows, snapped; anywhere else, nothing moves.
    pub(crate) fn drop_marker(&mut self, index: usize) {
        match self.marker_drag.take() {
            Some((dragged, at)) if dragged == index && self.saving.is_none() => {
                self.stop_typing();
                self.move_marker(index, at);
            }
            _ => {}
        }
    }

    /// The drag of a marker ended elsewhere: nothing moves; whether a
    /// ghost was showing.
    pub(crate) fn cancel_marker_drag(&mut self) -> bool {
        self.marker_drag.take().is_some()
    }

    /// A drag of chip `from` is over chip `over`, in its `half` (the left
    /// is [`Half::Upper`]): the insertion line goes where it would land.
    pub(crate) fn drag_chip(&mut self, from: usize, over: usize, half: Half) {
        let slot = drop_slot(over, half);
        self.chip_insertion = drop_index(from, slot, self.chips()).map(|_| (from, slot));
    }

    /// The drag of chip `from` was dropped on the chips: it moves where
    /// the insertion line shows ([`SongAction::DropChip`]).
    pub(crate) fn drop_chip(&mut self, from: usize) {
        let Some((dragged, slot)) = self.chip_insertion.take() else {
            return;
        };
        if dragged == from
            && let Some(to) = drop_index(from, slot, self.chips())
        {
            self.act(SongAction::DropChip { from, to });
        }
    }

    /// The drag of a chip ended elsewhere; whether a line was showing.
    pub(crate) fn cancel_chip_drag(&mut self) -> bool {
        self.chip_insertion.take().is_some()
    }

    /// The save failed with `message`: the editor stays with the reason.
    pub(crate) fn save_failed(&mut self, message: String) {
        self.saving = None;
        self.note = Some(message);
    }

    fn chips(&self) -> usize {
        self.shown_arrangement()
            .map_or(0, |arrangement| arrangement.sections.len())
    }

    fn move_marker(&mut self, index: usize, at: u64) {
        if let Some(id) = self
            .editor
            .package()
            .sections
            .get(index)
            .map(|m| m.id.clone())
        {
            let moved = self.editor.move_marker(&id, at, self.snap);
            if self.applied(moved).is_some() {
                self.select(index);
            }
        }
    }

    fn new_arrangement(&mut self) {
        let taken = |name: &str| {
            self.editor
                .package()
                .arrangements
                .iter()
                .any(|arrangement| arrangement.name.trim().to_lowercase() == name.to_lowercase())
        };
        let name = (1u64..)
            .map(|n| match n {
                1 => NEW_ARRANGEMENT_NAME.to_owned(),
                n => format!("{NEW_ARRANGEMENT_NAME} {n}"),
            })
            .find(|name| !taken(name))
            .expect("some numbered name is free");
        let created = self.editor.new_arrangement(&name);
        if let Some(id) = self.applied(created) {
            self.arrangement = id;
            self.start_rename();
        }
    }

    fn start_rename(&mut self) {
        if self.arrangement == DEFAULT_ARRANGEMENT_ID {
            // The domain says why: Default keeps its name.
            let refused = self
                .editor
                .rename_arrangement(DEFAULT_ARRANGEMENT_ID, "Default");
            self.applied(refused);
            return;
        }
        let Some(name) = self.shown_arrangement().map(|a| a.name.clone()) else {
            return;
        };
        self.rename = LineEdit::new(name);
        self.typing = Some(Typing::Rename);
    }

    fn delete_arrangement(&mut self) {
        let id = self.arrangement.clone();
        if id != DEFAULT_ARRANGEMENT_ID
            && let Some(names) = self.in_use.get(&id).filter(|names| !names.is_empty())
            && self.original.arrangements.iter().any(|a| a.id == id)
        {
            let name = self
                .shown_arrangement()
                .map_or(id.clone(), |a| a.name.clone());
            self.note = Some(in_use_note(&name, names));
            return;
        }
        let deleted = self.editor.delete_arrangement(&id);
        if self.applied(deleted).is_some() {
            self.arrangement = DEFAULT_ARRANGEMENT_ID.to_owned();
            self.chip_insertion = None;
        }
    }

    /// Keeps the result of an edit: the note clears, or says why it was
    /// refused.
    fn applied<T>(&mut self, result: Result<T, SongEditError>) -> Option<T> {
        match result {
            Ok(value) => {
                self.note = None;
                Some(value)
            }
            Err(error) => {
                self.note = Some(sentence(&error.to_string()));
                None
            }
        }
    }

    /// Edits the package outside the markers and arrangements (the maps, a
    /// stem's part), checking it again; a package that breaks a rule is
    /// refused with the note, and the edit stays as it was.
    fn rebuild(&mut self, change: impl FnOnce(&mut SongPackage)) {
        let mut package = self.editor.package().clone();
        change(&mut package);
        match SongEditor::new(package, self.editor.length_frames()) {
            Ok(editor) => {
                self.editor = editor;
                self.note = None;
            }
            Err(errors) => self.note = Some(package_problems(&errors)),
        }
    }

    /// Puts what the header's fields read into the edit: the title and key
    /// as they are, the tempo and meter as maps when they make some.
    fn sync_header(&mut self) {
        self.header.sync(&mut self.values);
        if self.maps_read_only {
            return;
        }
        let maps = BarMaps {
            bpm: self.values.bpm,
            time_signature: self.values.time_signature,
            changes: self.values.changes.clone(),
        };
        let package = self.editor.package();
        match maps.to_maps(package.sample_rate, self.editor.length_frames()) {
            Ok((tempo_map, time_signature_map)) => {
                self.map_problems.clear();
                if tempo_map != package.tempo_map
                    || time_signature_map != package.time_signature_map
                {
                    self.rebuild(|package| {
                        package.tempo_map = tempo_map;
                        package.time_signature_map = time_signature_map;
                    });
                    self.clamp_window();
                }
            }
            Err(errors) => {
                self.map_problems = errors.iter().map(|e| sentence(&e.to_string())).collect();
            }
        }
    }

    fn apply_number(&mut self) {
        let text = self.number.text().trim().to_owned();
        let number = if text.is_empty() {
            None
        } else {
            match text.parse::<u32>() {
                Ok(number) => Some(number),
                Err(_) => {
                    self.note = Some(format!(
                        "The number \"{text}\" is not a whole number, such as 2."
                    ));
                    return;
                }
            }
        };
        if let Some(marker) = self.selected_marker() {
            let (id, section_type) = (marker.id.clone(), marker.section_type);
            let set = self.editor.set_marker_type(&id, section_type, number);
            self.applied(set);
        }
    }

    fn stop_local_typing(&mut self) {
        if self.typing == Some(Typing::Number) {
            self.reset_number();
        }
        self.typing = None;
    }

    fn select(&mut self, index: usize) {
        let last = self.editor.package().sections.len().saturating_sub(1);
        self.selected = index.min(last);
        self.reset_number();
    }

    /// The number field holds the selected section's number again.
    fn reset_number(&mut self) {
        let number = self.selected_marker().and_then(|marker| marker.number);
        self.number = LineEdit::new(number.map(|n| n.to_string()).unwrap_or_default());
    }

    fn selected_marker(&self) -> Option<&domain::SectionMarker> {
        self.editor.package().sections.get(self.selected)
    }

    fn marker_index(&self, id: &str) -> Option<usize> {
        self.editor
            .package()
            .sections
            .iter()
            .position(|marker| marker.id == id)
    }

    fn shown_arrangement(&self) -> Option<&domain::Arrangement> {
        self.editor
            .package()
            .arrangements
            .iter()
            .find(|arrangement| arrangement.id == self.arrangement)
    }

    fn looped(&self, section: &str) -> bool {
        self.shown_arrangement()
            .is_some_and(|arrangement| arrangement.infinite_loop.iter().any(|id| id == section))
    }

    fn total_bars(&self) -> u64 {
        BarScale::of(&self.editor).bars().max(1)
    }

    fn visible_bars(&self) -> u64 {
        let total = self.total_bars();
        self.visible.unwrap_or(total).clamp(1, total)
    }

    fn window(&self) -> Window {
        let visible = self.visible_bars();
        let first = (self.scroll / 1000).min(self.total_bars() - visible);
        Window { first, visible }
    }

    /// Keeps the bars shown inside the song after its bar grid changed.
    fn clamp_window(&mut self) {
        let total = self.total_bars();
        if self.visible.is_some_and(|visible| visible >= total) {
            self.visible = None;
        }
        let room = total.saturating_sub(self.visible_bars()) * 1000;
        self.scroll = self.scroll.min(room);
    }

    fn zoom(&mut self, closer: bool) {
        let total = self.total_bars();
        let visible = self.visible_bars();
        let next = if closer {
            visible.div_ceil(2).max(MIN_VISIBLE_BARS.min(total))
        } else {
            visible.saturating_mul(2)
        };
        self.visible = (next < total).then_some(next);
        // The cursor stays in the middle of the bars shown.
        let next = self.visible_bars();
        let cursor = BarScale::of(&self.editor).units(self.cursor);
        let first = (cursor - next as f64 / 2.0).max(0.0) as u64;
        self.scroll = first.min(total - next) * 1000;
    }

    /// The frame `along` (out of [`crate::song_sections::ALONG`]) across the bars shown.
    fn frames_at(&self, along: u32) -> u64 {
        BarScale::of(&self.editor).frames(self.window().units_at(along))
    }

    fn start_save(&mut self, library: &Library) -> SaveSongJob {
        let ticket = next_ticket();
        self.saving = Some(ticket);
        let package = self.package();
        let deleted = self
            .original
            .arrangements
            .iter()
            .filter(|old| !package.arrangements.iter().any(|kept| kept.id == old.id))
            .map(|old| old.id.clone())
            .collect();
        SaveSongJob {
            ticket,
            library: library.clone(),
            song: self.song.clone(),
            package,
            length: self.editor.length_frames(),
            deleted,
        }
    }

    /// What keeps the edit from being saved, as sentences.
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.values.title.trim().is_empty() {
            problems.push(sentence(&domain::DraftError::EmptyTitle.to_string()));
        }
        if !self.maps_read_only {
            problems.extend(self.header.text_problems());
            problems.extend(self.map_problems.iter().cloned());
        }
        problems
    }
}

/// Why an arrangement the setlists `names` play cannot be deleted.
fn in_use_note(arrangement: &str, names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    let list = match quoted.as_slice() {
        [one] => format!("the setlist {one}"),
        [rest @ .., last] => format!("the setlists {} and {last}", rest.join(", ")),
        [] => "a setlist".to_owned(),
    };
    format!(
        "\"{arrangement}\" is played by {list}, so it cannot be deleted: choose another arrangement there first."
    )
}
