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

use domain::{Bus, Part, SongDraft, StoredFormat};
use library::{
    ImportDraft, ImportError, ImportProgress, Library, SetlistError, SongEntry, SongListing,
};

use crate::line_edit::LineKey;
use crate::mixer::stem_icon;
use crate::song_header::{HeaderFields, HeaderValues};

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

pub(crate) fn next_ticket() -> u64 {
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

/// Where a save job stands, so the editor can wait for it to stop. A
/// library song's save ([`crate::song_edit::SaveSongJob`]) keeps one too.
#[derive(Debug, Default)]
pub(crate) struct Stage {
    state: Mutex<Run>,
    changed: Condvar,
}

/// What a save job is doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Run {
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
    pub(crate) fn set(&self, run: Run) {
        *lock(&self.state) = run;
        self.changed.notify_all();
    }

    /// Waits until the job is not running, at most until `deadline`;
    /// whether it is not.
    pub(crate) fn wait(&self, deadline: Instant) -> bool {
        self.wait_while(deadline, |run| run == Run::Running)
    }

    /// Waits until the job is done, at most until `deadline`, for a job that
    /// cannot be cancelled and writes once it starts; whether it is done.
    pub(crate) fn wait_done(&self, deadline: Instant) -> bool {
        self.wait_while(deadline, |run| run != Run::Done)
    }

    fn wait_while(&self, deadline: Instant, waits: impl Fn(Run) -> bool) -> bool {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let (state, _) = self
            .changed
            .wait_timeout_while(lock(&self.state), timeout, |run| waits(*run))
            .unwrap_or_else(PoisonError::into_inner);
        !waits(*state)
    }
}

/// Marks its job done when dropped: when the job returns, panics, or is
/// dropped without running.
#[derive(Debug)]
pub(crate) struct Finish(pub(crate) Arc<Stage>);

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

/// The draft open in the Song editor, with the text of its fields.
#[derive(Debug)]
pub struct SongImport {
    draft: ImportDraft,
    header: HeaderFields,
    menu: Option<usize>,
    all_stems: bool,
    saving: Option<Saving>,
    save_problem: Option<String>,
}

impl SongImport {
    pub(crate) fn new(draft: ImportDraft) -> Self {
        Self {
            header: HeaderFields::new(&values_of(&draft.song)),
            menu: None,
            all_stems: false,
            saving: None,
            save_problem: None,
            draft,
        }
    }

    /// The field the keys type into, if any.
    pub(crate) fn typing(&self) -> Option<SongField> {
        self.header.typing()
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
            self.header.stop_typing();
        }
        if !matches!(action, ImportAction::TogglePartMenu(_)) {
            self.menu = None;
        }
        match action {
            ImportAction::Edit(field) => self.header.start_typing(field),
            ImportAction::AddChange => {
                self.header.add_change(&values_of(&self.draft.song));
                self.sync();
            }
            ImportAction::RemoveChange(index) => {
                // The draft's change goes too, so each change after it
                // keeps what it last read as.
                let mut values = values_of(&self.draft.song);
                if self.header.remove_change(index, &mut values) {
                    self.draft.song.changes = values.changes;
                    self.sync();
                }
            }
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
        if self.header.type_key(key) {
            self.sync();
        }
    }

    /// Text pasted into the field that has the focus.
    pub(crate) fn paste(&mut self, text: &str) {
        if self.header.paste(text) {
            self.sync();
        }
    }

    /// Leaves the field that has the focus, keeping its text.
    pub(crate) fn stop_typing(&mut self) {
        self.header.stop_typing();
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

    /// Puts what the fields read into the draft. A text that reads as no
    /// value leaves the draft's value as it was, and shows as a problem
    /// ([`Self::checks`]) that keeps the song from being saved.
    fn sync(&mut self) {
        let mut values = values_of(&self.draft.song);
        self.header.sync(&mut values);
        let song = &mut self.draft.song;
        song.title = values.title;
        song.key = values.key;
        song.bpm = values.bpm;
        song.time_signature = values.time_signature;
        song.changes = values.changes;
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
        let mut blocking = self.header.text_problems();
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

    /// What the Song editor shows.
    pub(crate) fn view(&self) -> SongEditorView {
        let song = &self.draft.song;
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
            title: self.header.field(SongField::Title),
            key: self.header.field(SongField::Key),
            tempo: self.header.field(SongField::Tempo),
            time: self.header.field(SongField::Time),
            changes: self.header.change_rows(),
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

/// The header values of `song`: its title, key, tempo and time signature
/// at the start, and its changes.
fn values_of(song: &SongDraft) -> HeaderValues {
    HeaderValues {
        title: song.title.clone(),
        key: song.key.clone(),
        bpm: song.bpm,
        time_signature: song.time_signature,
        changes: song.changes.clone(),
    }
}

/// A domain or library message as a sentence: capitalized, with a full
/// stop.
pub(crate) fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    full_stop(match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    })
}

/// `text` ending in a full stop.
pub(crate) fn full_stop(mut text: String) -> String {
    if !text.ends_with('.') {
        text.push('.');
    }
    text
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
pub(crate) fn kilohertz(rate: u32) -> String {
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
