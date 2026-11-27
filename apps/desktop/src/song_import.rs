//! The Song editor without a window (S7, F1): the stems of a folder or ZIP
//! become a song of the library folder. "Import stems" reads the source off
//! the UI thread ([`ProbeJob`]); the draft it gives opens in the Song editor
//! ([`SongImport`]), where the title, key, tempo, time signature, tempo or
//! meter changes and each stem's part are edited; "Save song" writes it off
//! the UI thread too ([`SaveJob`]), with its progress, and "Discard" drops
//! the draft or cancels the save. [`SongImport::view`] says what the editor
//! shows.
//!
//! The rules the draft must keep are the `domain` crate's
//! ([`domain::SongDraft::to_package`]) and the `library` crate's (a file
//! the import refuses blocks the save): every one the draft breaks shows,
//! in their words, and "Save song" is off until none is left. Text that is
//! no number or time signature at all shows the same way. A warning, such
//! as stem lengths that differ, shows and lets the song be saved.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use domain::{Bus, MapChange, Part, SongDraft, StoredFormat, TimeSignature};
use library::{
    ImportDraft, ImportError, ImportProgress, Library, SetlistError, SongEntry, SongListing,
};

use crate::line_edit::{LineEdit, LineKey, LineOutcome};
use crate::mixer::stem_icon;

/// The longest the app waits for a cancelled save to stop before it lets
/// go of it ([`crate::library_page::LibraryPage::stop_save`]): a save stops
/// within about a second of audio per stem being written, and then removes
/// its hidden folder.
pub const STOP_PATIENCE: Duration = Duration::from_secs(5);

/// How many bars after the last change "Tempo or meter change" adds the
/// next one, the first one after bar 1.
pub const CHANGE_STEP_BARS: u64 = 8;

/// Every job gets a ticket no other job of the process has, so the result
/// of a job a newer one replaced, or of another library's page, is never
/// taken for the current one.
static TICKETS: AtomicU64 = AtomicU64::new(1);

fn next_ticket() -> u64 {
    TICKETS.fetch_add(1, Ordering::Relaxed)
}

/// A control of the Song editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportAction {
    /// Starts typing into this field; the field typed into before keeps
    /// its text.
    Edit(SongField),
    /// "Tempo or meter change": adds a change [`CHANGE_STEP_BARS`] bars
    /// after the last one, at the tempo in force there.
    AddChange,
    /// Removes this change.
    RemoveChange(usize),
    /// The bus button of this stem: opens its part menu, or closes it.
    TogglePartMenu(usize),
    /// Closes the part menu.
    ClosePartMenu,
    /// Gives this stem this part, chosen in its menu, which closes.
    SetPart(usize, Part),
    /// "Show N more": every stem shows.
    ShowAllStems,
    /// "Save song": saves the draft off the UI thread, when nothing blocks
    /// it and no save runs.
    Save,
    /// "Discard": drops the draft, or cancels the save under way, and goes
    /// back to the Library once it has stopped.
    Discard,
}

/// A field of the Song editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongField {
    /// The song's title.
    Title,
    /// Its key, optional.
    Key,
    /// Its tempo at the start, in BPM.
    Tempo,
    /// Its time signature at the start, such as 4/4.
    Time,
    /// The bar this change starts.
    ChangeBar(usize),
    /// This change's tempo; empty keeps the tempo.
    ChangeBpm(usize),
    /// This change's time signature; empty keeps the meter.
    ChangeTime(usize),
}

/// Reads the folder or ZIP of an import ([`library::probe_import`]): run
/// off the UI thread, since a slow or sleeping drive can take seconds.
#[derive(Debug)]
pub struct ProbeJob {
    ticket: u64,
    source: PathBuf,
}

impl ProbeJob {
    pub(crate) fn new(source: &Path) -> Self {
        Self {
            ticket: next_ticket(),
            source: source.to_owned(),
        }
    }

    pub(crate) fn ticket(&self) -> u64 {
        self.ticket
    }

    /// Reads the source; it blocks until done.
    pub fn run(self) -> Probed {
        let result = library::probe_import(&self.source);
        Probed {
            ticket: self.ticket,
            result,
        }
    }
}

/// What a [`ProbeJob`] read, for [`crate::library_page::LibraryPage::probed`].
#[derive(Debug)]
pub struct Probed {
    pub(crate) ticket: u64,
    pub(crate) result: Result<ImportDraft, ImportError>,
}

/// How far a save has got, written by the save's thread and read by the
/// screen's.
type Progress = Arc<Mutex<Option<ImportProgress>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where a save job stands, so the editor can wait for it to stop.
#[derive(Debug, Default)]
struct Stage {
    state: Mutex<Run>,
    changed: Condvar,
}

/// What a save job is doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Run {
    /// It has not started: once its cancel flag is set, it writes nothing.
    #[default]
    Waiting,
    /// It runs, and may have written into its hidden folder.
    Running,
    /// It returned, or was dropped without running: nothing of it is left
    /// to remove.
    Done,
}

impl Stage {
    fn set(&self, run: Run) {
        *lock(&self.state) = run;
        self.changed.notify_all();
    }

    /// Waits until the job is not running, at most until `deadline`;
    /// whether it is not.
    fn wait(&self, deadline: Instant) -> bool {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let (state, _) = self
            .changed
            .wait_timeout_while(lock(&self.state), timeout, |run| *run == Run::Running)
            .unwrap_or_else(PoisonError::into_inner);
        *state != Run::Running
    }
}

/// Marks its job done when dropped: when the job returns, panics, or is
/// dropped without running.
#[derive(Debug)]
struct Finish(Arc<Stage>);

impl Drop for Finish {
    fn drop(&mut self) {
        self.0.set(Run::Done);
    }
}

/// Saves a draft as a song folder of the library ([`Library::save_import`])
/// and lists the library's songs again: run off the UI thread, since it
/// copies and transcodes every stem.
#[derive(Debug)]
pub struct SaveJob {
    ticket: u64,
    library: Library,
    draft: ImportDraft,
    progress: Progress,
    cancel: Arc<AtomicBool>,
    finish: Finish,
}

impl SaveJob {
    /// Saves the song, reporting its progress to the editor; it blocks
    /// until the song is saved, fails or is cancelled. On a cancel or a
    /// failure, it returns once it has removed what it wrote.
    pub fn run(self) -> Saved {
        let Self {
            ticket,
            library,
            draft,
            progress,
            cancel,
            finish,
        } = self;
        // Set under the stage's lock: a cancel set before it is seen by
        // the save, which then writes nothing.
        finish.0.set(Run::Running);
        let result = library.save_import(
            &draft,
            &mut |update| *lock(&progress) = Some(update),
            &cancel,
        );
        // The song list as it is now, the new song in it.
        let listing = result.is_ok().then(|| library.list_songs());
        let saved = Saved {
            ticket,
            root: library.root().to_owned(),
            result,
            listing,
        };
        drop(finish);
        saved
    }
}

/// What a [`SaveJob`] did, for [`crate::library_page::LibraryPage::saved`].
#[derive(Debug)]
pub struct Saved {
    pub(crate) ticket: u64,
    /// The library folder the song went into.
    pub(crate) root: PathBuf,
    pub(crate) result: Result<SongEntry, ImportError>,
    pub(crate) listing: Option<Result<SongListing, SetlistError>>,
}

/// What a Song editor control asks of the Library screen.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The editor stays.
    Stay,
    /// The draft is dropped: the Library shows again.
    Discarded,
    /// This save is to run off the UI thread.
    Save(Box<SaveJob>),
}

/// A save under way.
#[derive(Debug)]
pub(crate) struct Saving {
    pub(crate) ticket: u64,
    progress: Progress,
    cancel: Arc<AtomicBool>,
    stage: Arc<Stage>,
    /// Whether Discard cancelled it: once it stops, the Library shows.
    pub(crate) discarded: bool,
    /// The percentage last drawn ([`SongImport::progress_moved`]).
    drawn: Option<u32>,
}

impl Saving {
    fn percent(&self) -> Option<u32> {
        lock(&self.progress).map(|progress| (progress.fraction().clamp(0.0, 1.0) * 100.0) as u32)
    }

    /// Cancels the save, as Discard does: once it stops, the Library shows.
    fn cancel(&mut self) {
        self.discarded = true;
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Saving {
    /// A save the editor lets go of (another library opened, the window
    /// gone) is cancelled, and this waits, at most [`STOP_PATIENCE`], until
    /// it has stopped and removed what it wrote: the process may end next,
    /// and a save cut off by the end of the process would leave its hidden
    /// folder in the library.
    fn drop(&mut self) {
        self.cancel();
        self.stage.wait(Instant::now() + STOP_PATIENCE);
    }
}

/// The text of one tempo or meter change, and the change it last read as.
#[derive(Debug, Clone)]
struct ChangeEdit {
    bar: LineEdit,
    bpm: LineEdit,
    time: LineEdit,
}

/// The draft open in the Song editor, with the text of its fields.
#[derive(Debug)]
pub struct SongImport {
    draft: ImportDraft,
    title: LineEdit,
    key: LineEdit,
    tempo: LineEdit,
    time: LineEdit,
    changes: Vec<ChangeEdit>,
    typing: Option<SongField>,
    /// The text of the field typed into, as it was when typing started:
    /// Escape puts it back.
    before: String,
    menu: Option<usize>,
    all_stems: bool,
    saving: Option<Saving>,
    save_problem: Option<String>,
}

impl SongImport {
    pub(crate) fn new(draft: ImportDraft) -> Self {
        let song = &draft.song;
        Self {
            title: LineEdit::new(song.title.clone()),
            key: LineEdit::new(song.key.clone().unwrap_or_default()),
            tempo: LineEdit::new(number(song.bpm)),
            time: LineEdit::new(meter(song.time_signature)),
            changes: Vec::new(),
            typing: None,
            before: String::new(),
            menu: None,
            all_stems: false,
            saving: None,
            save_problem: None,
            draft,
        }
    }

    /// The field the keys type into, if any.
    pub(crate) fn typing(&self) -> Option<SongField> {
        self.typing
    }

    /// The save under way, if any.
    pub(crate) fn saving(&self) -> Option<&Saving> {
        self.saving.as_ref()
    }

    /// The draft's title, as it stands.
    pub(crate) fn title(&self) -> &str {
        &self.draft.song.title
    }

    /// Carries out `action`; a save goes into `library`.
    pub(crate) fn act(&mut self, action: ImportAction, library: &Library) -> Outcome {
        if self.saving.is_some() {
            // The save holds a copy of the draft: only Discard acts.
            if action == ImportAction::Discard
                && let Some(saving) = &mut self.saving
            {
                saving.cancel();
            }
            return Outcome::Stay;
        }
        if !matches!(action, ImportAction::Edit(_)) {
            self.typing = None;
        }
        if !matches!(action, ImportAction::TogglePartMenu(_)) {
            self.menu = None;
        }
        match action {
            ImportAction::Edit(field) => self.start_typing(field),
            ImportAction::AddChange => self.add_change(),
            ImportAction::RemoveChange(index) if index < self.changes.len() => {
                // The draft's change goes too, so each change after it
                // keeps what it last read as.
                self.changes.remove(index);
                if index < self.draft.song.changes.len() {
                    self.draft.song.changes.remove(index);
                }
                self.sync();
            }
            ImportAction::RemoveChange(_) => {}
            ImportAction::TogglePartMenu(index) => {
                self.menu = match self.menu {
                    Some(open) if open == index => None,
                    _ if index < self.draft.song.stems().len() => Some(index),
                    open => open,
                };
            }
            ImportAction::ClosePartMenu => {}
            ImportAction::SetPart(index, part) => {
                self.draft.song.set_part(index, part);
            }
            ImportAction::ShowAllStems => self.all_stems = true,
            ImportAction::Save => {
                if self.checks().blocking.is_empty() {
                    return Outcome::Save(Box::new(self.start_save(library)));
                }
            }
            ImportAction::Discard => return Outcome::Discarded,
        }
        Outcome::Stay
    }

    /// A key typed into the field that has the focus: the draft follows
    /// the text as it is typed.
    pub(crate) fn type_key(&mut self, key: LineKey<'_>) {
        let Some(field) = self.typing else {
            return;
        };
        let before = std::mem::take(&mut self.before);
        let Some(edit) = self.field_mut(field) else {
            self.typing = None;
            return;
        };
        match edit.key(key) {
            LineOutcome::Editing => self.before = before,
            LineOutcome::Commit(_) => self.typing = None,
            LineOutcome::Cancel => {
                *edit = LineEdit::new(before);
                self.typing = None;
            }
        }
        self.sync();
    }

    /// Text pasted into the field that has the focus.
    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(edit) = self.typing.and_then(|field| self.field_mut(field)) {
            edit.paste(text);
            self.sync();
        }
    }

    /// Leaves the field that has the focus, keeping its text.
    pub(crate) fn stop_typing(&mut self) {
        self.typing = None;
    }

    /// The save failed with `error`: the editor stays open with the
    /// reason, and Save song can be pressed again.
    pub(crate) fn save_failed(&mut self, error: &ImportError) {
        self.saving = None;
        self.save_problem = Some(full_stop(format!("The song could not be saved: {error}")));
    }

    /// Cancels the save under way, as Discard does, and waits at most
    /// `patience` until it has stopped and removed what it wrote; whether
    /// no save runs any more. The editor still waits for the save's result
    /// ([`crate::library_page::LibraryPage::saved`]).
    pub(crate) fn stop_save(&mut self, patience: Duration) -> bool {
        let Some(saving) = &mut self.saving else {
            return true;
        };
        saving.cancel();
        saving.stage.wait(Instant::now() + patience)
    }

    /// Whether the progress of the save under way changed since it was last
    /// drawn, which then counts as drawn.
    pub(crate) fn progress_moved(&mut self) -> bool {
        let Some(saving) = &mut self.saving else {
            return false;
        };
        let percent = saving.percent();
        let moved = percent != saving.drawn;
        saving.drawn = percent;
        moved
    }

    fn start_typing(&mut self, field: SongField) {
        if self.typing == Some(field) {
            // A click in the field typed into: Escape still puts back what
            // it held before.
            return;
        }
        let Some(edit) = self.field_mut(field) else {
            return;
        };
        let text = edit.text().to_owned();
        self.before = text;
        self.typing = Some(field);
    }

    fn field_mut(&mut self, field: SongField) -> Option<&mut LineEdit> {
        Some(match field {
            SongField::Title => &mut self.title,
            SongField::Key => &mut self.key,
            SongField::Tempo => &mut self.tempo,
            SongField::Time => &mut self.time,
            SongField::ChangeBar(index) => &mut self.changes.get_mut(index)?.bar,
            SongField::ChangeBpm(index) => &mut self.changes.get_mut(index)?.bpm,
            SongField::ChangeTime(index) => &mut self.changes.get_mut(index)?.time,
        })
    }

    fn add_change(&mut self) {
        let song = &self.draft.song;
        let last = song.changes.iter().max_by_key(|change| change.bar);
        let bar = last.map_or(1, |change| change.bar) + CHANGE_STEP_BARS;
        // The tempo in force where it goes: the last change's that sets
        // one, else the song's.
        let mut sorted: Vec<&MapChange> = song.changes.iter().collect();
        sorted.sort_by_key(|change| change.bar);
        let bpm = sorted
            .iter()
            .rev()
            .find_map(|change| change.bpm)
            .unwrap_or(song.bpm);
        self.changes.push(ChangeEdit {
            bar: LineEdit::new(bar.to_string()),
            bpm: LineEdit::new(number(bpm)),
            time: LineEdit::new(""),
        });
        self.sync();
    }

    /// Puts what the fields read into the draft. A text that reads as no
    /// value leaves the draft's value as it was, and shows as a problem
    /// ([`Self::checks`]) that keeps the song from being saved.
    fn sync(&mut self) {
        let song = &mut self.draft.song;
        song.title = self.title.text().to_owned();
        let key = self.key.text().trim();
        song.key = (!key.is_empty()).then(|| key.to_owned());
        if let Some(bpm) = parse_bpm(self.tempo.text()) {
            song.bpm = bpm;
        }
        if let Some(meter) = parse_meter(self.time.text()) {
            song.time_signature = meter;
        }
        let old = std::mem::take(&mut song.changes);
        song.changes = self
            .changes
            .iter()
            .enumerate()
            .map(|(index, edit)| {
                let before = old.get(index).copied();
                MapChange {
                    bar: parse_bar(edit.bar.text())
                        .or(before.map(|change| change.bar))
                        .unwrap_or(0),
                    bpm: optional(edit.bpm.text(), parse_bpm)
                        .unwrap_or_else(|| before.and_then(|change| change.bpm)),
                    time_signature: optional(edit.time.text(), parse_meter)
                        .unwrap_or_else(|| before.and_then(|change| change.time_signature)),
                }
            })
            .collect();
    }

    fn start_save(&mut self, library: &Library) -> SaveJob {
        let progress = Progress::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let stage = Arc::new(Stage::default());
        let ticket = next_ticket();
        self.save_problem = None;
        self.saving = Some(Saving {
            ticket,
            progress: Arc::clone(&progress),
            cancel: Arc::clone(&cancel),
            stage: Arc::clone(&stage),
            discarded: false,
            drawn: None,
        });
        SaveJob {
            ticket,
            library: library.clone(),
            draft: self.draft.clone(),
            progress,
            cancel,
            finish: Finish(stage),
        }
    }

    /// The problems that keep the draft from being saved and the warnings
    /// that do not, in the order the editor lists them.
    fn checks(&self) -> Checks {
        let mut blocking = self.text_problems();
        let mut warnings = Vec::new();
        match self.draft.song.to_package() {
            Ok(packaged) => {
                warnings.extend(packaged.warnings.iter().map(|w| sentence(&w.to_string())))
            }
            Err(errors) => blocking.extend(errors.iter().map(|error| sentence(&error.to_string()))),
        }
        let refused = self.draft.refused().len();
        if refused > 0 {
            let files = if refused == 1 {
                "1 file cannot be imported, so the song cannot be saved: take it out of the source"
                    .to_owned()
            } else {
                format!(
                    "{refused} files cannot be imported, so the song cannot be saved: take them out of the source"
                )
            };
            blocking.push(format!("{files} and import again."));
        }
        Checks { blocking, warnings }
    }

    /// The fields whose text reads as no value.
    fn text_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if parse_bpm(self.tempo.text()).is_none() {
            problems.push(not_a_tempo(self.tempo.text()));
        }
        if parse_meter(self.time.text()).is_none() {
            problems.push(not_a_meter(self.time.text()));
        }
        for edit in &self.changes {
            if parse_bar(edit.bar.text()).is_none() {
                problems.push(format!(
                    "The bar \"{}\" of a change is not a whole number, such as 17.",
                    edit.bar.text()
                ));
            }
            if optional(edit.bpm.text(), parse_bpm).is_none() {
                problems.push(not_a_tempo(edit.bpm.text()));
            }
            if optional(edit.time.text(), parse_meter).is_none() {
                problems.push(not_a_meter(edit.time.text()));
            }
        }
        problems
    }

    /// What the Song editor shows.
    pub(crate) fn view(&self) -> SongEditorView {
        let song = &self.draft.song;
        let field = |edit: &LineEdit, field: SongField| FieldView {
            text: edit.text().to_owned(),
            editing: self.typing == Some(field),
        };
        let stems = song
            .stems()
            .iter()
            .enumerate()
            .map(|(index, stem)| StemRow {
                index,
                file: stem.source_name.clone(),
                icon: stem_icon(
                    stem.part.role(),
                    stem.part.id(),
                    file_stem_name(&stem.source_name),
                ),
                part: stem.part,
                bus: stem.part.bus(),
                automatic: stem.automatic,
                menu_open: self.menu == Some(index),
            })
            .collect();
        let part_menu = self.menu.and_then(|index| {
            let current = song.stems().get(index)?.part;
            Some(PartMenu {
                stem: index,
                groups: Bus::ALL
                    .iter()
                    .map(|&bus| PartGroup {
                        bus,
                        parts: Part::ALL
                            .iter()
                            .filter(|part| part.bus() == bus)
                            .map(|&part| PartChoice {
                                part,
                                selected: part == current,
                            })
                            .collect(),
                    })
                    .collect(),
            })
        });
        let checks = self.checks();
        let saving = self.saving.as_ref().map(|saving| {
            let percent = saving.percent().unwrap_or(0);
            SavingView {
                label: if saving.discarded {
                    "Cancelling…".to_owned()
                } else {
                    format!("Saving… {percent}%")
                },
                percent,
                cancelling: saving.discarded,
            }
        });
        SongEditorView {
            source: self.draft.source().display().to_string(),
            title: field(&self.title, SongField::Title),
            key: field(&self.key, SongField::Key),
            tempo: field(&self.tempo, SongField::Tempo),
            time: field(&self.time, SongField::Time),
            changes: self
                .changes
                .iter()
                .enumerate()
                .map(|(index, edit)| ChangeRow {
                    index,
                    bar: field(&edit.bar, SongField::ChangeBar(index)),
                    bpm: field(&edit.bpm, SongField::ChangeBpm(index)),
                    time: field(&edit.time, SongField::ChangeTime(index)),
                })
                .collect(),
            stems_summary: stems_summary(song),
            stems,
            all_stems: self.all_stems,
            part_menu,
            refused: self
                .draft
                .refused()
                .iter()
                .map(|file| RefusedRow {
                    file: file.name.clone(),
                    reason: file.reason.clone(),
                })
                .collect(),
            can_save: checks.blocking.is_empty() && saving.is_none(),
            problems: checks.blocking,
            warnings: checks.warnings,
            saving,
            save_problem: self.save_problem.clone(),
        }
    }
}

struct Checks {
    blocking: Vec<String>,
    warnings: Vec<String>,
}

/// The name of a stem's file in the source, without folders or extension,
/// which picks its icon when its part names none.
fn file_stem_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// A domain or library message as a sentence: capitalized, with a full
/// stop.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    full_stop(match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    })
}

/// `text` ending in a full stop.
fn full_stop(mut text: String) -> String {
    if !text.ends_with('.') {
        text.push('.');
    }
    text
}

fn not_a_tempo(text: &str) -> String {
    format!("The tempo \"{text}\" is not a number of beats per minute, such as 120.")
}

fn not_a_meter(text: &str) -> String {
    format!("The time signature \"{text}\" is not written like 4/4.")
}

/// A tempo typed, with a point or a comma before its decimals; `None` for
/// anything but a number.
fn parse_bpm(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|bpm| bpm.is_finite())
}

/// A time signature typed as "6/8".
fn parse_meter(text: &str) -> Option<TimeSignature> {
    let (numerator, denominator) = text.trim().split_once('/')?;
    Some(TimeSignature {
        numerator: numerator.trim().parse().ok()?,
        denominator: denominator.trim().parse().ok()?,
    })
}

fn parse_bar(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// What an optional field reads as: `Some(None)` when empty, `Some(value)`
/// when it reads as one, `None` when it reads as none.
fn optional<T>(text: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Option<T>> {
    if text.trim().is_empty() {
        Some(None)
    } else {
        parse(text).map(Some)
    }
}

/// A tempo as typed: "120", "104.5".
fn number(bpm: f64) -> String {
    bpm.to_string()
}

fn meter(meter: TimeSignature) -> String {
    format!("{}/{}", meter.numerator, meter.denominator)
}

/// The line of the stems panel's header, as the design writes it: the
/// stem count, the song's sample rate in kHz and the format the stems are
/// stored in, "16 · 48 kHz · FLAC", or "WAV + FLAC" when they are mixed.
pub fn stems_summary(song: &SongDraft) -> String {
    let count = song.stems().len();
    let Some(rate) = song.sample_rate() else {
        return "No stems".to_owned();
    };
    let has = |format| song.stems().iter().any(|stem| stem.stored_as == format);
    let format = match (has(StoredFormat::Wav), has(StoredFormat::Flac)) {
        (true, true) => "WAV + FLAC",
        (true, false) => "WAV",
        _ => "FLAC",
    };
    format!("{count} · {} · {format}", kilohertz(rate))
}

/// A sample rate in kHz without trailing zeros: "48 kHz", "44.1 kHz".
fn kilohertz(rate: u32) -> String {
    let whole = rate / 1000;
    let rest = rate % 1000;
    if rest == 0 {
        return format!("{whole} kHz");
    }
    let decimals = format!("{rest:03}");
    format!("{whole}.{} kHz", decimals.trim_end_matches('0'))
}

/// A field as the editor shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldView {
    /// What it holds.
    pub text: String,
    /// Whether the keys type into it (it shows a caret).
    pub editing: bool,
}

/// A tempo or meter change, in its fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeRow {
    /// Its place among the changes.
    pub index: usize,
    /// The bar it starts, counted from 1.
    pub bar: FieldView,
    /// Its tempo in BPM; empty keeps the tempo.
    pub bpm: FieldView,
    /// Its time signature; empty keeps the meter.
    pub time: FieldView,
}

/// A stem of the Song editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemRow {
    /// Its place among the stems.
    pub index: usize,
    /// Its file's path in the source, such as "Guitars/EG 1.wav".
    pub file: String,
    /// Its icon ([`stem_icon`]).
    pub icon: &'static str,
    /// Its part.
    pub part: Part,
    /// The bus its part belongs to, which its bus button shows.
    pub bus: Bus,
    /// Whether the import chose the part from the file name (the AUTO
    /// badge); `false` once one is chosen.
    pub automatic: bool,
    /// Whether its part menu is open.
    pub menu_open: bool,
}

/// The parts of one bus, under its heading in the part menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartGroup {
    /// The bus.
    pub bus: Bus,
    /// Its parts, in [`Part::ALL`] order.
    pub parts: Vec<PartChoice>,
}

/// A part of the part menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartChoice {
    /// The part.
    pub part: Part,
    /// Whether the stem has it.
    pub selected: bool,
}

/// The part menu of a stem, open under its bus button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartMenu {
    /// The stem it is for.
    pub stem: usize,
    /// Every part ([`Part::ALL`]), grouped by bus in [`Bus::ALL`] order.
    pub groups: Vec<PartGroup>,
}

/// A save under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavingView {
    /// What the Save song button reads: "Saving… 42%", or "Cancelling…"
    /// once Discard was pressed.
    pub label: String,
    /// How much of the audio is written, 0 to 100.
    pub percent: u32,
    /// Whether Discard cancelled it.
    pub cancelling: bool,
}

/// A file of the source the import cannot save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedRow {
    /// Its path in the source.
    pub file: String,
    /// Why, such as "it has 6 channels; stems must be mono or stereo".
    pub reason: String,
}

/// Everything the Song editor shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SongEditorView {
    /// The folder or ZIP file the stems come from.
    pub source: String,
    /// The title.
    pub title: FieldView,
    /// The key.
    pub key: FieldView,
    /// The tempo at the start.
    pub tempo: FieldView,
    /// The time signature at the start.
    pub time: FieldView,
    /// The tempo and meter changes, in the order they were added.
    pub changes: Vec<ChangeRow>,
    /// The stems panel's header: "16 · 48 kHz · FLAC".
    pub stems_summary: String,
    /// Every stem, in source order.
    pub stems: Vec<StemRow>,
    /// Whether "Show N more" was pressed: every stem shows.
    pub all_stems: bool,
    /// The part menu, while one is open.
    pub part_menu: Option<PartMenu>,
    /// The files the import refuses, with the reason.
    pub refused: Vec<RefusedRow>,
    /// What keeps the song from being saved, as sentences.
    pub problems: Vec<String>,
    /// What does not, such as stem lengths that differ.
    pub warnings: Vec<String>,
    /// Whether Save song saves: nothing blocks it and no save runs.
    pub can_save: bool,
    /// The save under way.
    pub saving: Option<SavingView>,
    /// Why the last save failed; Save song can be pressed again.
    pub save_problem: Option<String>,
}
