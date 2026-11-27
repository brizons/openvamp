//! S4 with F5.3: a library song saved from the Song editor keeps every
//! setlist that plays part of one of its arrangements (a start or end
//! position) on the audio it played. A new marker or a reorder moves the
//! item's positions; positions the edit removes or plays in another order
//! ask before the save, which then has the item play the whole arrangement.
//! The jobs run on the test's thread (or another one), and what they write
//! is read back from disk. Adapted from the T3 verifier's probe.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use domain::{
    Arrangement, SectionMarker, SectionType, Setlist, SetlistItem, SongItem, SongPackage, Stem,
    StemRole, TempoPoint, TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::Library;
use openvamp_desktop::ending::{Dialog, SaveQuestion};
use openvamp_desktop::library_page::{LibraryAction, LibraryPage};
use openvamp_desktop::reorder::Half;
use openvamp_desktop::song_edit::{SaveSongJob, SongAction};
use openvamp_desktop::song_import::ImportAction;
use openvamp_desktop::song_sections::{ALONG, SongEditView};

const RATE: u32 = 8_000;

/// A bar at 120 BPM in 4/4.
const BAR: u64 = 2 * RATE as u64;

/// The song's bars.
const BARS: u64 = 20;

/// The song's folder in the library.
const SONG: &str = "Probe.song";

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|&id| id.to_owned()).collect()
}

fn marker(id: &str, section_type: SectionType, number: Option<u32>, bar: u64) -> SectionMarker {
    SectionMarker {
        id: id.to_owned(),
        section_type,
        number,
        start: (bar - 1) * BAR,
    }
}

/// "Probe": six sections; Default plays them once, Live (positions 0–9)
/// plays the chorus five times, Short the verse and the chorus, and Encore
/// the chorus and the outro.
fn package() -> SongPackage {
    let arrangement = |id: &str, name: &str, sections: &[&str]| Arrangement {
        id: id.to_owned(),
        name: name.to_owned(),
        sections: ids(sections),
        infinite_loop: Vec::new(),
    };
    SongPackage {
        format_version: 1,
        title: "Probe".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: None,
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: vec![Stem {
            id: "keys".to_owned(),
            file: "keys.wav".to_owned(),
            part: "keys".to_owned(),
            role: StemRole::Track,
        }],
        sections: vec![
            marker("intro", SectionType::Intro, None, 1),
            marker("verse", SectionType::Verse, Some(1), 3),
            marker("chorus", SectionType::Chorus, None, 5),
            marker("verse-2", SectionType::Verse, Some(2), 9),
            marker("bridge", SectionType::Bridge, None, 13),
            marker("outro", SectionType::Outro, None, 17),
        ],
        arrangements: vec![
            arrangement(
                "default",
                "Default",
                &["intro", "verse", "chorus", "verse-2", "bridge", "outro"],
            ),
            arrangement(
                "live",
                "Live",
                &[
                    "intro", "verse", "chorus", "chorus", "verse-2", "chorus", "bridge", "chorus",
                    "chorus", "outro",
                ],
            ),
            arrangement("short", "Short", &["verse", "chorus"]),
            arrangement("encore", "Encore", &["chorus", "outro"]),
        ],
    }
}

fn wav(path: &Path) {
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).unwrap();
    for _ in 0..BARS * BAR {
        writer.write_sample(1_000i16).unwrap();
    }
    writer.finalize().unwrap();
}

/// A library holding "Probe", the setlist "Gig", which plays Live's
/// positions 2 to 3 (its first two choruses), and "Sunday", which plays
/// all of Short.
struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(SONG);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("song.json"), package().to_json().unwrap()).unwrap();
        wav(&folder.join("keys.wav"));
        let library = Library::new(dir.path());
        let mut gig = Setlist::new("Gig");
        gig.items = vec![SetlistItem::Song(SongItem {
            arrangement: "live".to_owned(),
            start_position: Some(2),
            end_position: Some(3),
            ..SongItem::new(SONG)
        })];
        library.save_setlist(&gig).unwrap();
        let mut sunday = Setlist::new("Sunday");
        sunday.items = vec![SetlistItem::Song(SongItem {
            arrangement: "short".to_owned(),
            ..SongItem::new(SONG)
        })];
        library.save_setlist(&sunday).unwrap();
        Self { dir }
    }

    fn library(&self) -> Library {
        Library::new(self.dir.path())
    }

    fn setlists(&self) -> PathBuf {
        self.dir.path().join("Setlists")
    }

    fn page(&self) -> LibraryPage {
        LibraryPage::open(self.dir.path(), None).unwrap()
    }

    fn saved(&self) -> SongPackage {
        let json = fs::read_to_string(self.dir.path().join(SONG).join("song.json")).unwrap();
        SongPackage::from_json(&json).unwrap()
    }

    /// The setlist `name` as saved.
    fn setlist(&self, name: &str) -> Setlist {
        let library = self.library();
        let file = library
            .list_setlists()
            .unwrap()
            .setlists
            .into_iter()
            .find(|entry| entry.name == name)
            .unwrap()
            .file;
        library.load_setlist(&file).unwrap()
    }

    /// Gig's item: its start and end, and the sections it plays, once the
    /// setlist checks against the library.
    fn gig(&self) -> (Option<usize>, Option<usize>, Vec<String>) {
        let setlist = self.setlist("Gig");
        let songs = self.library().setlist_songs(&setlist).unwrap();
        setlist.validate(&songs).expect("Gig checks");
        let SetlistItem::Song(item) = &setlist.items[0] else {
            unreachable!("Gig plays a song")
        };
        let live = self
            .saved()
            .arrangements
            .into_iter()
            .find(|a| a.id == "live")
            .unwrap();
        let start = item.start_position.unwrap_or(0);
        let end = item.end_position.unwrap_or(live.sections.len() - 1);
        (
            item.start_position,
            item.end_position,
            live.sections[start..=end].to_vec(),
        )
    }
}

fn open(page: &mut LibraryPage) {
    let row = page
        .view(None)
        .songs
        .iter()
        .position(|song| song.title == "Probe")
        .unwrap();
    page.act(LibraryAction::EditSong(row));
    let job = page.take_open().unwrap();
    page.song_opened(job.run());
    let job = page.take_waveform().unwrap();
    page.waveform_done(job.run());
}

fn opened(bench: &Bench) -> LibraryPage {
    let mut page = bench.page();
    open(&mut page);
    page
}

fn view(page: &LibraryPage) -> SongEditView {
    page.song_edit_view().expect("the Song editor is open")
}

fn song(page: &mut LibraryPage, action: SongAction) {
    page.act(LibraryAction::Song(action));
}

fn show_live(page: &mut LibraryPage) {
    song(page, SongAction::SelectArrangement(1));
    assert_eq!(view(page).arrangement.tabs[1].name, "Live");
}

/// "Save song": the save job, when it starts without a question.
fn press_save(page: &mut LibraryPage) -> Option<SaveSongJob> {
    page.act(LibraryAction::Import(ImportAction::Save));
    page.take_song_save()
}

fn save(page: &mut LibraryPage) {
    let job = press_save(page).expect("the save starts at once");
    page.song_saved(job.run());
}

/// The chip at `from` dropped on the `half` of the chip at `over`.
fn drag_chip(page: &mut LibraryPage, from: usize, over: usize, half: Half) {
    page.chip_drag_over(from, over, half);
    page.chip_drop(from);
}

fn chips(page: &LibraryPage) -> Vec<String> {
    view(page)
        .arrangement
        .chips
        .iter()
        .map(|chip| chip.abbr.clone())
        .collect()
}

#[test]
fn a_new_marker_moves_a_setlists_positions_onto_the_audio_they_played() {
    let bench = Bench::new();
    assert_eq!(bench.gig(), (Some(2), Some(3), ids(&["chorus", "chorus"])));
    let mut page = opened(&bench);

    // A marker at bar 4, inside Verse 1 (bars 3 and 4), which Live plays
    // before its choruses.
    let along = (3.0 / BARS as f64 * f64::from(ALONG)).round() as u32;
    song(&mut page, SongAction::PlaceCursor(along));
    song(&mut page, SongAction::AddMarker);
    save(&mut page);

    assert!(page.song_edit_view().is_none(), "saved without a question");
    assert_eq!(bench.gig(), (Some(3), Some(4), ids(&["chorus", "chorus"])));
    assert_eq!(page.view(None).warning, None);
}

#[test]
fn a_marker_that_splits_a_section_the_setlist_plays_keeps_both_parts_in_its_range() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    // Bar 7, inside the chorus.
    let along = (6.0 / BARS as f64 * f64::from(ALONG)).round() as u32;
    song(&mut page, SongAction::PlaceCursor(along));
    song(&mut page, SongAction::AddMarker);
    save(&mut page);

    assert_eq!(
        bench.gig(),
        (
            Some(2),
            Some(5),
            ids(&["chorus", "chorus-2", "chorus", "chorus-2"])
        )
    );
}

#[test]
fn chips_reordered_around_a_setlists_range_shift_it() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    show_live(&mut page);

    // The intro goes after the outro.
    drag_chip(&mut page, 0, 9, Half::Lower);
    assert_eq!(chips(&page)[0], "V1");
    save(&mut page);

    assert_eq!(bench.gig(), (Some(1), Some(2), ids(&["chorus", "chorus"])));
}

#[test]
fn chips_reordered_inside_a_setlists_range_ask_before_the_save() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    show_live(&mut page);

    // The two choruses Gig plays swap places.
    drag_chip(&mut page, 2, 3, Half::Lower);

    assert!(press_save(&mut page).is_none(), "the save waits");
    let question = page.take_save_question(Dialog::Mac).expect("the save asks");
    assert_eq!(
        question.detail,
        "\"Gig\" plays positions 3 to 4 of Live, which this edit removes or plays in another order: it will play all of Live."
    );
}

#[test]
fn a_chip_moved_into_a_setlists_range_asks_before_the_save() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    show_live(&mut page);

    // Verse 2 goes between the two choruses Gig plays.
    drag_chip(&mut page, 4, 3, Half::Upper);
    assert_eq!(chips(&page)[2..5], ["C", "V2", "C"]);

    assert!(press_save(&mut page).is_none(), "the save waits");
    assert!(page.take_save_question(Dialog::Mac).is_some());
}

#[test]
fn positions_a_setlist_plays_that_the_edit_removes_ask_first_and_then_play_the_whole_arrangement() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    show_live(&mut page);
    for _ in 0..8 {
        song(&mut page, SongAction::RemoveChip(0));
    }
    assert_eq!(chips(&page), ["C", "Out"]);

    assert!(press_save(&mut page).is_none(), "the save waits");
    let question = page.take_save_question(Dialog::Mac).unwrap();
    assert!(page.take_save_question(Dialog::Mac).is_none(), "asked once");
    assert_eq!(
        question.message,
        "This save changes setlists that play part of the song"
    );
    assert_eq!(
        question.detail,
        "\"Gig\" plays positions 3 to 4 of Live, which this edit removes or plays in another order: it will play all of Live."
    );
    let labels: Vec<&str> = question.answers.iter().map(|a| a.label).collect();
    assert_eq!(labels, ["Save anyway", "Keep editing"]);
    assert!(question.saves(Some(0)));
    assert!(!question.saves(Some(1)) && !question.saves(None));
    // Windows lists the cancel answer first, so Enter keeps editing.
    let windows = SaveQuestion::new(question.detail.clone(), Dialog::Windows);
    assert_eq!(windows.answers[0].label, "Keep editing");

    // Keep editing: nothing is written, the edits stay.
    page.save_answered(false);
    assert!(page.take_song_save().is_none());
    assert_eq!(chips(&page), ["C", "Out"]);
    assert_eq!(bench.saved(), package());

    // Save anyway: Gig plays all of Live, and the Library says so.
    assert!(press_save(&mut page).is_none());
    assert!(page.take_save_question(Dialog::Mac).is_some());
    page.save_answered(true);
    let job = page.take_song_save().expect("the save starts");
    page.song_saved(job.run());
    assert!(page.song_edit_view().is_none());
    assert_eq!(bench.gig(), (None, None, ids(&["chorus", "outro"])));
    assert_eq!(
        page.view(None).warning.as_deref(),
        Some("\"Gig\" now plays all of Live.")
    );
    // Sunday plays all of Short, which the edit did not touch.
    let SetlistItem::Song(sunday) = &bench.setlist("Sunday").items[0] else {
        unreachable!()
    };
    assert_eq!((sunday.start_position, sunday.end_position), (None, None));
}

#[test]
fn the_setlist_open_in_the_library_is_read_again_after_the_save() {
    let bench = Bench::new();
    let mut page = bench.page();
    let row = page
        .view(None)
        .setlists
        .iter()
        .position(|s| s.name == "Gig")
        .unwrap();
    page.act(LibraryAction::SelectSetlist(row));
    open(&mut page);
    let along = (3.0 / BARS as f64 * f64::from(ALONG)).round() as u32;
    song(&mut page, SongAction::PlaceCursor(along));
    song(&mut page, SongAction::AddMarker);

    save(&mut page);

    let editor = page.editor().expect("Gig is in the setlist editor");
    let SetlistItem::Song(item) = &editor.setlist().items[0] else {
        unreachable!()
    };
    assert_eq!((item.start_position, item.end_position), (Some(3), Some(4)));
    assert!(editor.problems().is_empty());
    assert_eq!(
        page.view(None).editor.unwrap().rows[0].length,
        "0:16",
        "two choruses of 4 bars"
    );
}

#[test]
fn a_setlist_that_cannot_be_written_is_named_and_the_song_stays_saved() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    let along = (3.0 / BARS as f64 * f64::from(ALONG)).round() as u32;
    song(&mut page, SongAction::PlaceCursor(along));
    song(&mut page, SongAction::AddMarker);
    let setlists = bench.setlists();
    let mut permissions = fs::metadata(&setlists).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&setlists, permissions.clone()).unwrap();

    save(&mut page);

    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&setlists, permissions).unwrap();
    assert!(page.song_edit_view().is_none(), "the song is saved");
    assert_eq!(bench.saved().sections.len(), 7);
    let warning = page.view(None).warning.expect("a warning names Gig");
    assert!(
        warning.starts_with("\"Gig\" could not be updated to the edit: "),
        "{warning}"
    );
    let SetlistItem::Song(item) = &bench.setlist("Gig").items[0] else {
        unreachable!()
    };
    assert_eq!((item.start_position, item.end_position), (Some(2), Some(3)));
}

#[test]
fn an_arrangement_is_not_deleted_while_a_setlist_cannot_be_read() {
    let bench = Bench::new();
    fs::write(bench.setlists().join("Broken.setlist.json"), "{ not json").unwrap();
    let mut page = opened(&bench);
    // Encore is in none of the setlists that can be read.
    song(&mut page, SongAction::SelectArrangement(3));
    song(&mut page, SongAction::DeleteArrangement);

    assert_eq!(
        view(&page).editor.save_problem.as_deref(),
        Some(
            "\"Encore\" cannot be deleted: Broken.setlist.json could not be read, so whether a setlist plays it is not known."
        )
    );
    assert_eq!(view(&page).arrangement.tabs.len(), 4);
    // A new arrangement is in no setlist, whatever they hold.
    song(&mut page, SongAction::NewArrangement);
    page.stop_typing();
    song(&mut page, SongAction::DeleteArrangement);
    assert_eq!(view(&page).arrangement.tabs.len(), 4);
    assert_eq!(view(&page).editor.save_problem, None);
}

#[test]
fn quitting_waits_a_bounded_time_for_a_song_save_in_flight() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    song(&mut page, SongAction::AppendSection(0));
    assert!(!page.song_saving());

    let job = press_save(&mut page).unwrap();
    assert!(page.song_saving(), "a library song is being saved");
    // Never run: the wait ends at its bound.
    let started = Instant::now();
    assert!(!page.stop_save(Duration::from_millis(100)));
    assert!(started.elapsed() >= Duration::from_millis(100));
    // Run on another thread a little later: the wait ends when it is done.
    let (sender, done) = mpsc::channel();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        sender.send(job.run()).unwrap();
    });
    let started = Instant::now();
    assert!(page.stop_save(Duration::from_secs(10)));
    assert!(started.elapsed() >= Duration::from_millis(250));
    worker.join().unwrap();
    page.song_saved(done.recv().unwrap());
    assert!(!page.song_saving());
    assert_eq!(bench.saved().arrangements[0].sections.len(), 7);
}

#[test]
fn the_delete_marker_hint_says_why_a_marker_cannot_be_deleted() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    let first = view(&page).inspector;
    assert!(!first.can_delete);
    assert_eq!(
        first.delete_hint.as_deref(),
        Some("The first marker starts the song")
    );
    // Short plays only the chorus once its verse goes.
    song(&mut page, SongAction::SelectArrangement(2));
    song(&mut page, SongAction::RemoveChip(0));
    song(&mut page, SongAction::SelectSection(2));

    let chorus = view(&page).inspector;
    assert_eq!(chorus.name, "Chorus");
    assert!(!chorus.can_delete);
    assert_eq!(
        chorus.delete_hint.as_deref(),
        Some("\"Chorus\" is all that Short plays")
    );
    song(&mut page, SongAction::SelectSection(1));
    assert!(view(&page).inspector.can_delete);
    assert_eq!(view(&page).inspector.delete_hint, None);
}
