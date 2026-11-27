//! S1–S4 (F2): the Song editor of a song already in the library, without a
//! window, on a temporary library folder. The read, the waveform and the
//! save run as the jobs the screen hands to its background executor; here
//! they run on the test's thread, and what they write is read back from
//! disk.

use std::fs;
use std::path::{Path, PathBuf};

use domain::{
    Arrangement, ArrangementMix, MixFile, Part, SectionMarker, SectionType, Setlist, SetlistItem,
    SongItem, SongPackage, Stem, StemMix, StemRole, TempoPoint, TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::{Library, OverviewError};
use openvamp_desktop::ending::{Dialog, Leaving};
use openvamp_desktop::library_page::{LibraryAction, LibraryPage};
use openvamp_desktop::line_edit::LineKey;
use openvamp_desktop::reorder::Half;
use openvamp_desktop::song_edit::{LOOP_HINT, READ_ONLY_MAPS, SongAction};
use openvamp_desktop::song_import::{ImportAction, SongField};
use openvamp_desktop::song_sections::{ALONG, SongEditView, WAVE_HIGH, WAVE_LOW, WaveformState};
use openvamp_desktop::view::SectionKind;

const RATE: u32 = 8_000;

/// A bar at 120 BPM in 4/4: four half-second beats.
const BAR: u64 = 2 * RATE as u64;

/// The song's bars.
const BARS: u64 = 16;

/// The level of the keys stem in bar `index`, counted from 0: louder bar
/// after bar.
fn level(index: u64) -> f32 {
    0.1 + 0.05 * index as f32
}

/// A mono 16-bit WAV of `BARS` bars, each sample of bar `i` at `levels(i)`.
fn wav(path: &Path, levels: impl Fn(u64) -> f32) {
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).unwrap();
    for frame in 0..BARS * BAR {
        let value = (levels(frame / BAR) * f32::from(i16::MAX)) as i16;
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn marker(id: &str, section_type: SectionType, number: Option<u32>, bar: u64) -> SectionMarker {
    SectionMarker {
        id: id.to_owned(),
        section_type,
        number,
        start: (bar - 1) * BAR,
    }
}

fn arrangement(id: &str, name: &str, sections: &[&str]) -> Arrangement {
    Arrangement {
        id: id.to_owned(),
        name: name.to_owned(),
        sections: sections.iter().map(|&s| s.to_owned()).collect(),
        infinite_loop: Vec::new(),
    }
}

/// "First Light": 16 bars at 120 BPM in 4/4, an intro (bar 1), Verse 1
/// (bar 5), a chorus (bar 9) and an outro (bar 13); Default plays them
/// all, Short the verse and the chorus, Long all and the chorus twice. A
/// keys stem gets louder bar by bar; the click is loud and not in the
/// waveform.
fn package() -> SongPackage {
    SongPackage {
        format_version: 1,
        title: "First Light".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: Some("G".to_owned()),
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: vec![
            Stem {
                id: "keys".to_owned(),
                file: "keys.wav".to_owned(),
                part: "keys".to_owned(),
                role: StemRole::Track,
            },
            Stem {
                id: "click".to_owned(),
                file: "click.wav".to_owned(),
                part: "click".to_owned(),
                role: StemRole::Click,
            },
        ],
        sections: vec![
            marker("intro", SectionType::Intro, None, 1),
            marker("verse", SectionType::Verse, Some(1), 5),
            marker("chorus", SectionType::Chorus, None, 9),
            marker("outro", SectionType::Outro, None, 13),
        ],
        arrangements: vec![
            arrangement("default", "Default", &["intro", "verse", "chorus", "outro"]),
            arrangement("short", "Short", &["verse", "chorus"]),
            arrangement(
                "long",
                "Long",
                &["intro", "verse", "chorus", "chorus", "outro"],
            ),
        ],
    }
}

/// A library folder holding "First Light" (as `package` gives it) and the
/// setlist "Sunday", which plays its Short arrangement.
struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        Self::with(package())
    }

    fn with(package: SongPackage) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bench = Self { dir };
        let folder = bench.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("song.json"), package.to_json().unwrap()).unwrap();
        wav(&folder.join("keys.wav"), level);
        wav(&folder.join("click.wav"), |_| 0.9);
        let mut setlist = Setlist::new("Sunday");
        setlist.items = vec![SetlistItem::Song(SongItem {
            arrangement: "short".to_owned(),
            ..SongItem::new("First Light.song")
        })];
        Library::new(bench.dir.path())
            .save_setlist(&setlist)
            .unwrap();
        bench
    }

    fn folder(&self) -> PathBuf {
        self.dir.path().join("First Light.song")
    }

    fn page(&self) -> LibraryPage {
        LibraryPage::open(self.dir.path(), None).unwrap()
    }

    /// The `song.json` on disk.
    fn saved(&self) -> SongPackage {
        SongPackage::from_json(&fs::read_to_string(self.folder().join("song.json")).unwrap())
            .unwrap()
    }
}

/// The edit link of `title`'s row, and the read run as the screen runs it.
fn open(page: &mut LibraryPage, title: &str) {
    let row = page
        .view(None)
        .songs
        .iter()
        .position(|song| song.title == title)
        .expect("the song is listed");
    page.act(LibraryAction::EditSong(row));
    let job = page.take_open().expect("the edit link reads the song");
    page.song_opened(job.run());
}

/// "First Light" open in the Song editor of a new page, its waveform read.
fn opened(bench: &Bench) -> LibraryPage {
    let mut page = bench.page();
    open(&mut page, "First Light");
    draw_waveform(&mut page);
    page
}

fn draw_waveform(page: &mut LibraryPage) {
    let job = page.take_waveform().expect("the editor reads its waveform");
    page.waveform_done(job.run());
}

fn view(page: &LibraryPage) -> SongEditView {
    page.song_edit_view().expect("the Song editor is open")
}

fn song(page: &mut LibraryPage, action: SongAction) {
    page.act(LibraryAction::Song(action));
}

fn header(page: &mut LibraryPage, action: ImportAction) {
    page.act(LibraryAction::Import(action));
}

/// Where `bars` bars from the song start are across the bars shown.
fn along(page: &LibraryPage, bars: f64) -> u32 {
    let markers = view(page).markers;
    let first = (markers.first_bar - 1) as f64;
    ((bars - first) / markers.visible_bars as f64 * f64::from(ALONG)).round() as u32
}

/// A click `bars` bars from the song start on the waveform.
fn place_cursor(page: &mut LibraryPage, bars: f64) {
    let along = along(page, bars);
    song(page, SongAction::PlaceCursor(along));
}

/// A drag of marker `index` over the waveform, `bars` bars from the start.
fn drag_over(page: &mut LibraryPage, index: usize, bars: f64) {
    let along = along(page, bars);
    page.marker_drag_over(index, along);
}

/// The tab of the arrangement `name`.
fn show(page: &mut LibraryPage, name: &str) {
    let index = tab(page, name);
    song(page, SongAction::SelectArrangement(index));
}

fn note(page: &LibraryPage) -> Option<String> {
    view(page).editor.save_problem
}

fn abbreviations(page: &LibraryPage) -> Vec<String> {
    view(page)
        .markers
        .blocks
        .iter()
        .map(|block| block.abbr.clone())
        .collect()
}

fn chips(page: &LibraryPage) -> Vec<String> {
    view(page)
        .arrangement
        .chips
        .iter()
        .map(|chip| chip.abbr.clone())
        .collect()
}

fn tab(page: &LibraryPage, name: &str) -> usize {
    view(page)
        .arrangement
        .tabs
        .iter()
        .position(|tab| tab.name == name)
        .unwrap_or_else(|| panic!("{name} is a tab"))
}

/// Clicks `field` and types `text` in place of what it holds, then Enter.
fn type_into(page: &mut LibraryPage, field: SongField, text: &str) {
    header(page, ImportAction::Edit(field));
    let editor = view(page).editor;
    let old = match field {
        SongField::Title => editor.title.text,
        SongField::Key => editor.key.text,
        SongField::Tempo => editor.tempo.text,
        SongField::Time => editor.time.text,
        SongField::ChangeBar(index) => editor.changes[index].bar.text.clone(),
        SongField::ChangeBpm(index) => editor.changes[index].bpm.text.clone(),
        SongField::ChangeTime(index) => editor.changes[index].time.text.clone(),
    };
    for _ in old.chars() {
        page.type_key(LineKey::Backspace);
    }
    page.type_key(LineKey::Text(text));
    page.type_key(LineKey::Enter);
}

/// "Save song", and the save run as the screen runs it.
fn save(page: &mut LibraryPage) {
    header(page, ImportAction::Save);
    let job = page.take_song_save().expect("Save song starts a save");
    page.song_saved(job.run());
}

#[test]
fn a_song_rows_edit_link_reads_the_song_off_the_ui_thread_and_opens_the_song_editor() {
    let bench = Bench::new();
    let mut page = bench.page();
    let row = page.view(None).songs[0].index;

    page.act(LibraryAction::EditSong(row));

    assert_eq!(
        page.view(None).importing.as_deref(),
        Some("Opening First Light…")
    );
    assert!(
        page.song_edit_view().is_none(),
        "nothing opens before the read"
    );
    let job = page.take_open().expect("the read runs off the UI thread");
    assert!(page.take_open().is_none(), "once");
    page.song_opened(job.run());
    let editor = view(&page).editor;
    assert_eq!(page.view(None).importing, None);
    assert_eq!(editor.title.text, "First Light");
    assert_eq!(editor.key.text, "G");
    assert_eq!(editor.tempo.text, "120");
    assert_eq!(editor.time.text, "4/4");
    assert!(editor.changes.is_empty());
    assert!(editor.can_save);
    assert!(page.song_editor().is_none(), "no import is open");
}

#[test]
fn the_editor_shows_the_songs_sections_on_the_strip_and_its_arrangements_as_tabs() {
    let bench = Bench::new();
    let page = opened(&bench);

    let shown = view(&page);
    let markers = &shown.markers;
    assert_eq!(markers.total_bars, BARS);
    assert_eq!((markers.first_bar, markers.visible_bars), (1, BARS));
    assert_eq!(abbreviations(&page), ["In", "V1", "C", "Out"]);
    let starts: Vec<&str> = markers.blocks.iter().map(|b| b.starts.as_str()).collect();
    assert_eq!(starts, ["Bar 1", "Bar 5", "Bar 9", "Bar 13"]);
    let spans: Vec<(f32, f32)> = markers
        .blocks
        .iter()
        .map(|b| (b.span.left, b.span.width))
        .collect();
    assert_eq!(
        spans,
        [(0.0, 0.25), (0.25, 0.25), (0.5, 0.25), (0.75, 0.25)]
    );
    let kinds: Vec<SectionKind> = markers.blocks.iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        [
            SectionKind::Neutral,
            SectionKind::Verse,
            SectionKind::Chorus,
            SectionKind::Neutral
        ]
    );
    assert!(!markers.blocks[0].movable && markers.blocks[1].movable);
    // A label every 8 bars.
    let ruler: Vec<u64> = markers.ruler.iter().map(|mark| mark.bar).collect();
    assert_eq!(ruler, [1, 9]);
    assert_eq!(markers.ruler[1].left, 0.5);
    let tabs: Vec<(&str, bool)> = shown
        .arrangement
        .tabs
        .iter()
        .map(|tab| (tab.name.as_str(), tab.active))
        .collect();
    assert_eq!(tabs, [("Default", true), ("Short", false), ("Long", false)]);
    assert_eq!(chips(&page), ["In", "V1", "C", "Out"]);
    let palette: Vec<&str> = shown
        .arrangement
        .palette
        .iter()
        .map(|p| p.label.as_str())
        .collect();
    assert_eq!(palette, ["Intro", "Verse 1", "Chorus", "Outro"]);
    // The first section is selected.
    assert_eq!(shown.inspector.name, "Intro");
    assert_eq!(markers.selection.unwrap().width, 0.25);
}

#[test]
fn a_click_on_the_waveform_places_the_cursor_on_the_nearest_bar_or_beat() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    // 2.6 bars in: between bar 3 (2 bars in) and bar 4 (3 bars in).
    place_cursor(&mut page, 2.6);
    let markers = view(&page).markers;
    assert_eq!(markers.cursor_label, "Bar 4");
    assert_eq!(markers.cursor, Some(3.0 / BARS as f32));

    song(&mut page, SongAction::ToggleSnap);
    assert!(!view(&page).markers.snap_to_bar);
    place_cursor(&mut page, 2.6);
    // The beats of bar 3 are 0.25 bar apart: 2.6 is nearest its third.
    assert_eq!(view(&page).markers.cursor_label, "Bar 3.3");
}

#[test]
fn add_marker_at_playhead_splits_the_section_under_the_cursor_in_every_arrangement() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    place_cursor(&mut page, 6.0);

    song(&mut page, SongAction::AddMarker);

    assert_eq!(abbreviations(&page), ["In", "V1", "V", "C", "Out"]);
    let shown = view(&page);
    assert_eq!(shown.markers.blocks[2].starts, "Bar 7");
    assert_eq!(shown.inspector.name, "Verse", "the new section is selected");
    assert_eq!(chips(&page), ["In", "V1", "V", "C", "Out"]);
    show(&mut page, "Short");
    assert_eq!(chips(&page), ["V1", "V", "C"]);
    assert_eq!(note(&page), None);
}

#[test]
fn a_marker_added_where_one_starts_is_refused_with_the_domains_words() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    place_cursor(&mut page, 4.2);

    song(&mut page, SongAction::AddMarker);

    assert_eq!(
        note(&page).as_deref(),
        Some("\"Verse 1\" already starts at bar 5, beat 1; two markers cannot share a start.")
    );
    assert_eq!(abbreviations(&page), ["In", "V1", "C", "Out"]);
    // The next edit that applies clears the note.
    place_cursor(&mut page, 2.0);
    song(&mut page, SongAction::AddMarker);
    assert_eq!(note(&page), None);
}

#[test]
fn a_dragged_marker_moves_on_release_snapped_to_the_bar_or_the_beat() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    drag_over(&mut page, 2, 10.3);
    let ghost = view(&page).markers.drag.expect("the ghost shows");
    assert_eq!((ghost.index, ghost.label.as_str()), (2, "Bar 11"));
    assert_eq!(
        view(&page).markers.blocks[2].starts,
        "Bar 9",
        "nothing moves before the release"
    );
    page.marker_drop(2);
    let shown = view(&page);
    assert_eq!(shown.markers.blocks[2].starts, "Bar 11");
    assert_eq!(shown.markers.drag, None);
    assert_eq!(
        shown.inspector.name, "Chorus",
        "the moved section is selected"
    );
    assert_eq!(shown.inspector.length, "2 bars");

    song(&mut page, SongAction::ToggleSnap);
    drag_over(&mut page, 2, 11.3);
    page.marker_drop(2);
    assert_eq!(view(&page).markers.blocks[2].starts, "Bar 12.2");

    // A drag that ends anywhere else moves nothing.
    drag_over(&mut page, 2, 13.6);
    assert!(page.cancel_marker_drag());
    page.marker_drop(2);
    assert_eq!(view(&page).markers.blocks[2].starts, "Bar 12.2");
}

#[test]
fn a_marker_moved_past_its_neighbour_or_the_first_one_is_refused_as_a_note() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    drag_over(&mut page, 1, 10.0);
    page.marker_drop(1);

    assert_eq!(
        note(&page).as_deref(),
        Some(
            "\"Verse 1\" cannot move to or past \"Chorus\"; a marker stays between its neighbours."
        )
    );
    assert_eq!(view(&page).markers.blocks[1].starts, "Bar 5");
    // The first marker starts the song: no ghost, nothing moves.
    drag_over(&mut page, 0, 2.0);
    assert_eq!(view(&page).markers.drag, None);
    let along = along(&page, 2.0);
    song(&mut page, SongAction::MoveMarker { index: 0, along });
    assert_eq!(
        note(&page).as_deref(),
        Some("The first marker starts the song; it cannot be moved.")
    );
}

#[test]
fn the_inspector_shows_the_selected_sections_type_number_start_and_length() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    song(&mut page, SongAction::SelectSection(1));

    let inspector = view(&page).inspector;
    assert_eq!(inspector.name, "Verse 1");
    assert_eq!(inspector.section_type, "Verse");
    assert_eq!(inspector.number.text, "1");
    assert_eq!(inspector.starts_at, "Bar 5");
    assert_eq!(inspector.length, "4 bars");
    assert_eq!(inspector.arrangement, "Default");
    assert!(!inspector.infinite_loop);
    assert_eq!(inspector.loop_hint, LOOP_HINT);
    assert!(inspector.can_delete);
    assert_eq!(inspector.kind, SectionKind::Verse);
    song(&mut page, SongAction::SelectSection(0));
    assert!(!view(&page).inspector.can_delete);
    assert_eq!(view(&page).inspector.number.text, "");
}

#[test]
fn the_type_menu_lists_the_built_in_types_and_gives_the_section_one() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    song(&mut page, SongAction::SelectSection(1));

    song(&mut page, SongAction::ToggleTypeMenu);

    let menu = view(&page).inspector.type_menu.expect("the menu is open");
    let names: Vec<&str> = menu.iter().map(|choice| choice.label.as_str()).collect();
    let all: Vec<&str> = SectionType::ALL.iter().map(|t| t.name()).collect();
    assert_eq!(names, all);
    let selected: Vec<&str> = menu
        .iter()
        .filter(|c| c.selected)
        .map(|c| c.label.as_str())
        .collect();
    assert_eq!(selected, ["Verse"]);
    let chorus = SectionType::ALL
        .iter()
        .position(|&t| t == SectionType::Chorus)
        .unwrap();
    song(&mut page, SongAction::SetType(chorus));
    let shown = view(&page);
    assert_eq!(shown.inspector.name, "Chorus 1");
    assert_eq!(shown.inspector.type_menu, None, "the menu closes");
    assert_eq!(shown.markers.blocks[1].abbr, "C1");
    assert_eq!(shown.arrangement.chips[1].abbr, "C1");
}

#[test]
fn typing_a_number_numbers_the_section_and_text_that_is_none_is_a_note() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    song(&mut page, SongAction::SelectSection(1));

    song(&mut page, SongAction::EditNumber);
    assert!(page.takes_keys());
    assert!(view(&page).inspector.number.editing);
    page.type_key(LineKey::Backspace);
    assert_eq!(view(&page).inspector.name, "Verse", "no number");
    page.type_key(LineKey::Text("2"));
    assert_eq!(view(&page).inspector.name, "Verse 2");
    page.type_key(LineKey::Text("x"));
    assert_eq!(
        note(&page).as_deref(),
        Some("The number \"2x\" is not a whole number, such as 2.")
    );
    assert_eq!(view(&page).inspector.name, "Verse 2", "it does not apply");
    // Escape puts back what it held before typing started.
    page.type_key(LineKey::Escape);
    assert!(!page.takes_keys());
    assert_eq!(view(&page).inspector.name, "Verse 1");
    assert_eq!(note(&page), None);
}

#[test]
fn infinite_loop_by_default_switches_for_the_arrangement_shown() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    song(&mut page, SongAction::SelectSection(1));

    song(&mut page, SongAction::ToggleLoop);

    assert!(view(&page).inspector.infinite_loop);
    assert!(view(&page).arrangement.chips[1].looped);
    show(&mut page, "Short");
    assert!(
        !view(&page).inspector.infinite_loop,
        "Short does not loop it"
    );
    assert_eq!(view(&page).inspector.arrangement, "Short");
    // Short does not play the intro: the switch is refused.
    song(&mut page, SongAction::SelectSection(0));
    assert_eq!(view(&page).inspector.loop_hint, "Not in Short");
    song(&mut page, SongAction::ToggleLoop);
    assert_eq!(
        note(&page).as_deref(),
        Some("\"Intro\" is not in arrangement \"Short\"; only its own sections can loop.")
    );
    show(&mut page, "Default");
    song(&mut page, SongAction::SelectSection(1));
    song(&mut page, SongAction::ToggleLoop);
    assert!(!view(&page).inspector.infinite_loop, "switched off again");
}

#[test]
fn delete_marker_takes_its_section_out_of_every_arrangement_but_not_the_first_one() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    song(&mut page, SongAction::SelectSection(1));

    song(&mut page, SongAction::DeleteMarker);

    assert_eq!(abbreviations(&page), ["In", "C", "Out"]);
    assert_eq!(view(&page).inspector.name, "Intro", "the one before shows");
    assert_eq!(view(&page).inspector.length, "8 bars");
    assert_eq!(chips(&page), ["In", "C", "Out"]);
    show(&mut page, "Short");
    assert_eq!(chips(&page), ["C"]);
    song(&mut page, SongAction::SelectSection(0));
    song(&mut page, SongAction::DeleteMarker);
    assert_eq!(
        note(&page).as_deref(),
        Some("The first marker starts the song; it cannot be deleted.")
    );
    // Deleting the chorus would leave Short empty.
    song(&mut page, SongAction::SelectSection(1));
    song(&mut page, SongAction::DeleteMarker);
    assert_eq!(
        note(&page).as_deref(),
        Some("Deleting \"Chorus\" would leave arrangement \"Short\" with no sections.")
    );
    assert_eq!(abbreviations(&page), ["In", "C", "Out"]);
}

#[test]
fn new_makes_an_arrangement_of_every_section_and_renames_it_in_place() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    song(&mut page, SongAction::NewArrangement);

    let shown = view(&page).arrangement;
    let new = shown.tabs.last().unwrap();
    assert_eq!(new.name, "New arrangement");
    assert!(new.active);
    assert_eq!(
        new.renaming.as_ref().map(|field| field.text.as_str()),
        Some("New arrangement")
    );
    assert!(shown.editable);
    assert_eq!(chips(&page), ["In", "V1", "C", "Out"]);
    for _ in "New arrangement".chars() {
        page.type_key(LineKey::Backspace);
    }
    page.type_key(LineKey::Text("Long ending"));
    page.type_key(LineKey::Enter);
    let tab = view(&page).arrangement.tabs.last().cloned().unwrap();
    assert_eq!((tab.name.as_str(), tab.renaming), ("Long ending", None));
    // A name another arrangement has is refused.
    song(&mut page, SongAction::RenameArrangement);
    for _ in "Long ending".chars() {
        page.type_key(LineKey::Backspace);
    }
    page.type_key(LineKey::Text("short"));
    page.type_key(LineKey::Enter);
    assert_eq!(
        note(&page).as_deref(),
        Some("An arrangement named \"Short\" already exists.")
    );
    assert_eq!(
        view(&page).arrangement.tabs.last().unwrap().name,
        "Long ending"
    );
    song(&mut page, SongAction::DeleteArrangement);
    let tabs: Vec<String> = view(&page)
        .arrangement
        .tabs
        .iter()
        .map(|tab| tab.name.clone())
        .collect();
    assert_eq!(tabs, ["Default", "Short", "Long"]);
    assert!(view(&page).arrangement.tabs[0].active, "Default shows");
}

#[test]
fn the_default_arrangement_cannot_be_renamed_or_deleted() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    assert!(!view(&page).arrangement.editable);

    song(&mut page, SongAction::RenameArrangement);
    assert_eq!(
        note(&page).as_deref(),
        Some("The Default arrangement cannot be renamed or deleted.")
    );
    assert!(!page.takes_keys());
    song(&mut page, SongAction::SelectSection(1));
    song(&mut page, SongAction::DeleteArrangement);

    assert_eq!(
        note(&page).as_deref(),
        Some("The Default arrangement cannot be renamed or deleted.")
    );
    assert_eq!(view(&page).arrangement.tabs.len(), 3);
}

#[test]
fn an_arrangement_a_setlist_plays_cannot_be_deleted_and_the_note_names_the_setlist() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    show(&mut page, "Short");

    song(&mut page, SongAction::DeleteArrangement);

    assert_eq!(
        note(&page).as_deref(),
        Some(
            "\"Short\" is played by the setlist \"Sunday\", so it cannot be deleted: choose another arrangement there first."
        )
    );
    assert_eq!(view(&page).arrangement.tabs.len(), 3);
    // Long is in no setlist.
    show(&mut page, "Long");
    song(&mut page, SongAction::DeleteArrangement);
    assert_eq!(view(&page).arrangement.tabs.len(), 2);
    assert_eq!(note(&page), None);
}

#[test]
fn chips_reorder_by_drag_and_leave_with_their_cross() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    // The intro dragged over the right half of the chorus.
    page.chip_drag_over(0, 2, Half::Lower);
    assert_eq!(view(&page).arrangement.insertion, Some(3));
    page.chip_drop(0);
    assert_eq!(chips(&page), ["V1", "C", "In", "Out"]);
    assert_eq!(view(&page).arrangement.insertion, None);
    // Dropping right next to itself moves nothing.
    page.chip_drag_over(1, 1, Half::Lower);
    assert_eq!(view(&page).arrangement.insertion, None);

    song(&mut page, SongAction::RemoveChip(0));
    assert_eq!(chips(&page), ["C", "In", "Out"]);
    show(&mut page, "Short");
    song(&mut page, SongAction::RemoveChip(0));
    song(&mut page, SongAction::RemoveChip(0));
    assert_eq!(chips(&page), ["C"]);
    assert_eq!(
        note(&page).as_deref(),
        Some("Arrangement \"Short\" needs at least one section.")
    );
}

#[test]
fn a_section_added_to_the_end_plays_last_and_the_summary_counts_it() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    song(&mut page, SongAction::AppendSection(2));

    assert_eq!(chips(&page), ["In", "V1", "C", "Out", "C"]);
    let shown = view(&page).arrangement;
    assert_eq!(shown.summary, "5 sections · 20 bars · 0:40");
    assert_eq!(
        (
            shown.sections.as_str(),
            shown.bars.as_str(),
            shown.time.as_str()
        ),
        ("5", "20", "0:40")
    );
}

#[test]
fn the_summary_reads_sections_bars_and_length_of_the_arrangement_shown() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    assert_eq!(
        view(&page).arrangement.summary,
        "4 sections · 16 bars · 0:32"
    );
    show(&mut page, "Short");
    assert_eq!(
        view(&page).arrangement.summary,
        "2 sections · 8 bars · 0:16"
    );
    show(&mut page, "Long");
    let shown = view(&page).arrangement;
    assert_eq!(shown.summary, "5 sections · 20 bars · 0:40");
    let bars: Vec<&str> = shown.chips.iter().map(|chip| chip.bars.as_str()).collect();
    assert_eq!(bars, ["4", "4", "4", "4", "4"]);
}

#[test]
fn the_waveform_draws_a_bar_per_bar_as_tall_as_its_level_once_the_overview_is_read() {
    let bench = Bench::new();
    let mut page = bench.page();
    open(&mut page, "First Light");

    let flat = view(&page).markers;
    assert_eq!(flat.waveform, WaveformState::Reading { percent: 0 });
    assert_eq!(flat.bars.len(), BARS as usize);
    assert!(flat.bars.iter().all(|bar| bar.height == WAVE_LOW));
    draw_waveform(&mut page);

    let markers = view(&page).markers;
    assert_eq!(markers.waveform, WaveformState::Ready);
    // The overview the job cached, built from the stems by the library.
    let overview = library::load_overview(&bench.folder()).expect("the job cached it");
    let peak = overview.peak();
    assert!(peak > 0.0);
    let heights: Vec<f32> = markers.bars.iter().map(|bar| bar.height).collect();
    let expected: Vec<f32> = (0..BARS)
        .map(|index| {
            let level = overview.range_level(index * BAR..(index + 1) * BAR);
            WAVE_LOW + (WAVE_HIGH - WAVE_LOW) * (level / peak)
        })
        .collect();
    assert_eq!(heights, expected);
    // The loudest bar is the last; the click stem is not in it.
    assert!((heights[BARS as usize - 1] - WAVE_HIGH).abs() < 1e-6);
    assert!(heights[0] < heights[1]);
    let kinds: Vec<SectionKind> = markers.bars.iter().map(|bar| bar.kind).collect();
    assert_eq!(
        kinds[3..6],
        [SectionKind::Neutral, SectionKind::Verse, SectionKind::Verse]
    );
}

#[test]
fn a_tempo_edit_redraws_the_bars_from_the_same_overview_and_markers_keep_their_audio() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    let overview = library::load_overview(&bench.folder()).unwrap();

    type_into(&mut page, SongField::Tempo, "60");

    let markers = view(&page).markers;
    // At 60 BPM a bar lasts 4 s: 8 bars, each two of the old ones.
    assert_eq!(markers.total_bars, BARS / 2);
    let heights: Vec<f32> = markers.bars.iter().map(|bar| bar.height).collect();
    let expected: Vec<f32> = (0..BARS / 2)
        .map(|index| {
            let level = overview.range_level(index * 2 * BAR..(index + 1) * 2 * BAR);
            WAVE_LOW + (WAVE_HIGH - WAVE_LOW) * (level / overview.peak())
        })
        .collect();
    assert_eq!(heights, expected);
    assert!(page.take_waveform().is_none(), "nothing is read again");
    // The verse still starts 8 s in: bar 3 at 60 BPM.
    assert_eq!(markers.blocks[1].starts, "Bar 3");
}

#[test]
fn tempo_and_meter_changes_edit_the_saved_maps_and_read_back_as_the_same_changes() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    header(&mut page, ImportAction::AddChange);
    type_into(&mut page, SongField::ChangeBpm(0), "90");
    type_into(&mut page, SongField::ChangeTime(0), "3/4");
    save(&mut page);

    assert!(page.song_edit_view().is_none(), "saved, the Library shows");
    let saved = bench.saved();
    // Bar 9 starts 8 bars of 2 s in.
    assert_eq!(
        saved.tempo_map,
        [
            TempoPoint { at: 0, bpm: 120.0 },
            TempoPoint {
                at: 8 * BAR,
                bpm: 90.0
            }
        ]
    );
    assert_eq!(
        saved.time_signature_map[1],
        TimeSignaturePoint {
            at: 8 * BAR,
            numerator: 3,
            denominator: 4
        }
    );
    // The markers stay on their samples.
    assert_eq!(saved.sections, package().sections);
    open(&mut page, "First Light");
    let changes = view(&page).editor.changes;
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (
            changes[0].bar.text.as_str(),
            changes[0].bpm.text.as_str(),
            changes[0].time.text.as_str()
        ),
        ("9", "90", "3/4")
    );
    assert!(!page.has_unsaved_song(), "read back as saved");
}

#[test]
fn a_change_the_maps_refuse_shows_and_keeps_the_song_from_being_saved() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    header(&mut page, ImportAction::AddChange);
    type_into(&mut page, SongField::ChangeBar(0), "40");

    let editor = view(&page).editor;
    assert_eq!(
        editor.problems,
        ["The change at bar 40 is past the end of the song, whose last bar is bar 16."]
    );
    assert!(!editor.can_save);
    header(&mut page, ImportAction::RemoveChange(0));
    assert!(view(&page).editor.can_save);
}

#[test]
fn a_song_whose_changes_do_not_start_bars_shows_its_maps_read_only_and_saves_them_as_they_are() {
    let mut off_bar = package();
    off_bar.tempo_map.push(TempoPoint {
        at: BAR + 100,
        bpm: 90.0,
    });
    let bench = Bench::with(off_bar.clone());
    let mut page = opened(&bench);

    let shown = view(&page);
    assert!(shown.maps_read_only);
    assert_eq!(shown.editor.warnings, [READ_ONLY_MAPS]);
    assert_eq!(shown.editor.tempo.text, "120");
    header(&mut page, ImportAction::Edit(SongField::Tempo));
    assert!(!view(&page).editor.tempo.editing, "Tempo is read-only");
    header(&mut page, ImportAction::AddChange);
    assert!(view(&page).editor.changes.is_empty());
    type_into(&mut page, SongField::Title, "First Light II");
    save(&mut page);

    let saved = bench.saved();
    assert_eq!(saved.title, "First Light II");
    assert_eq!(saved.tempo_map, off_bar.tempo_map);
}

#[test]
fn save_song_rewrites_song_json_and_the_library_shows_the_song_again() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    type_into(&mut page, SongField::Title, "First Light (live)");
    type_into(&mut page, SongField::Key, "A");
    place_cursor(&mut page, 10.0);
    song(&mut page, SongAction::AddMarker);
    show(&mut page, "Long");
    song(&mut page, SongAction::RemoveChip(3));
    song(&mut page, SongAction::SelectSection(2));
    song(&mut page, SongAction::ToggleLoop);
    assert!(page.has_unsaved_song());

    header(&mut page, ImportAction::Save);
    let saving = view(&page).editor;
    assert_eq!(saving.saving.map(|s| s.label).as_deref(), Some("Saving…"));
    assert!(!saving.can_save);
    let job = page
        .take_song_save()
        .expect("the save runs off the UI thread");
    page.song_saved(job.run());

    assert!(page.song_edit_view().is_none());
    let saved = bench.saved();
    let mut expected = package();
    expected.title = "First Light (live)".to_owned();
    expected.key = Some("A".to_owned());
    expected
        .sections
        .insert(3, marker("chorus-2", SectionType::Chorus, None, 11));
    let ids = |ids: &[&str]| ids.iter().map(|&id| id.to_owned()).collect::<Vec<_>>();
    // The new marker splits the chorus in every arrangement, after each of
    // its turns; the fourth position of Long went, and its chorus loops.
    expected.arrangements[0].sections = ids(&["intro", "verse", "chorus", "chorus-2", "outro"]);
    expected.arrangements[1].sections = ids(&["verse", "chorus", "chorus-2"]);
    expected.arrangements[2].sections =
        ids(&["intro", "verse", "chorus", "chorus", "chorus-2", "outro"]);
    expected.arrangements[2].infinite_loop = ids(&["chorus"]);
    assert_eq!(saved, expected);
    let library = page.view(None);
    assert_eq!(library.songs[0].title, "First Light (live)");
    assert_eq!(library.songs[0].key, "A");
    assert_eq!(page.imported_row(), Some(0), "the song saved is marked");
}

#[test]
fn a_save_that_fails_keeps_the_editor_open_with_the_reason() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    type_into(&mut page, SongField::Title, "Renamed");
    let manifest = bench.folder().join("song.json");
    let mut permissions = fs::metadata(&manifest).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&manifest, permissions.clone()).unwrap();

    save(&mut page);

    let editor = view(&page).editor;
    let problem = editor.save_problem.expect("the reason shows");
    assert!(
        problem.starts_with("The song could not be saved: cannot write "),
        "{problem}"
    );
    assert!(editor.can_save, "it can be saved again");
    assert_eq!(bench.saved().title, "First Light");
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&manifest, permissions).unwrap();
    save(&mut page);
    assert_eq!(bench.saved().title, "Renamed");
}

#[test]
fn discard_drops_the_edits_once_the_question_is_answered() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    type_into(&mut page, SongField::Title, "Renamed");
    song(&mut page, SongAction::SelectSection(1));
    song(&mut page, SongAction::DeleteMarker);

    header(&mut page, ImportAction::Discard);
    assert_eq!(page.take_leave_question(), Some(Leaving::Discard));
    page.leave_answered(true);

    assert!(page.song_edit_view().is_none(), "the Library shows");
    assert_eq!(bench.saved(), package(), "nothing was written");
    // Opened again, the song is as it was.
    open(&mut page, "First Light");
    assert_eq!(view(&page).editor.title.text, "First Light");
    assert_eq!(abbreviations(&page), ["In", "V1", "C", "Out"]);
}

#[test]
fn leaving_with_edits_not_saved_asks_first_and_keep_editing_keeps_them() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    // Nothing edited: Discard and the Library link leave at once.
    header(&mut page, ImportAction::Discard);
    assert!(page.song_edit_view().is_none());
    assert_eq!(page.take_leave_question(), None);
    open(&mut page, "First Light");
    song(&mut page, SongAction::AppendSection(0));
    assert!(page.has_unsaved_song());

    header(&mut page, ImportAction::Discard);

    assert!(page.song_edit_view().is_some(), "the editor waits");
    assert_eq!(page.take_leave_question(), Some(Leaving::Discard));
    assert_eq!(page.take_leave_question(), None, "asked once");
    let question = Leaving::Discard.question(Dialog::Mac);
    let labels: Vec<&str> = question.answers.iter().map(|a| a.label).collect();
    assert_eq!(labels, ["Discard changes", "Keep editing"]);
    assert!(question.ends(Some(0)));
    assert!(!question.ends(Some(1)) && !question.ends(None));
    page.leave_answered(false);
    assert_eq!(
        chips(&page),
        ["In", "V1", "C", "Out", "In"],
        "the edits stay"
    );
    assert!(page.has_unsaved_song());
}

#[test]
fn the_question_before_quitting_or_opening_another_library_drops_the_edits_on_its_first_mac_answer()
{
    for leaving in [
        Leaving::OpenLibrary,
        Leaving::End(openvamp_desktop::ending::Ending::Quit),
        Leaving::End(openvamp_desktop::ending::Ending::CloseWindow),
    ] {
        let mac = leaving.question(Dialog::Mac);
        assert_eq!(mac.message, "The song has edits that are not saved");
        assert_eq!(mac.answers[0].label, "Discard changes");
        assert!(mac.answers[1].keeps_playing);
        // Windows lists the cancel answer first.
        let windows = leaving.question(Dialog::Windows);
        assert_eq!(windows.answers[0].label, "Keep editing");
        assert!(windows.ends(Some(1)));
    }
    let bench = Bench::new();
    let mut page = opened(&bench);
    song(&mut page, SongAction::AppendSection(0));
    page.discard_song_edit();
    assert!(page.song_edit_view().is_none());
    assert!(!page.has_unsaved_song());
}

#[test]
fn closing_the_editor_cancels_its_waveform_build() {
    let bench = Bench::new();
    let mut page = bench.page();
    open(&mut page, "First Light");
    let job = page.take_waveform().expect("the editor reads its waveform");

    header(&mut page, ImportAction::Discard);
    assert!(page.song_edit_view().is_none());
    let done = job.run();

    assert!(
        matches!(done.overview(), Err(OverviewError::Cancelled)),
        "the build was cancelled"
    );
    assert!(
        !bench.folder().join("waveforms/overview.bin").exists(),
        "nothing was cached"
    );
    page.waveform_done(done);
    assert!(page.song_edit_view().is_none());
}

#[test]
fn deleting_an_arrangement_drops_its_mix_from_mix_json_once_the_song_is_saved() {
    let bench = Bench::new();
    let mut mix = MixFile::new();
    let muted = StemMix {
        mute: true,
        ..StemMix::default()
    };
    for id in ["default", "long"] {
        let mut arrangement = ArrangementMix::default();
        arrangement.set_stem("keys", muted.clone());
        mix.set_arrangement(id, arrangement);
    }
    library::write_mix(&bench.folder(), &mix).unwrap();
    let mut page = opened(&bench);
    show(&mut page, "Long");
    song(&mut page, SongAction::DeleteArrangement);

    save(&mut page);

    let json = fs::read_to_string(bench.folder().join("mix.json")).unwrap();
    let kept = MixFile::from_json(&json).unwrap();
    let ids: Vec<&str> = kept.arrangements.keys().map(String::as_str).collect();
    assert_eq!(ids, ["default"]);
    assert_eq!(page.view(None).warning, None);
}

#[test]
fn the_stems_panel_lists_each_stems_part_and_its_menu_changes_it() {
    let bench = Bench::new();
    let mut page = opened(&bench);

    let editor = view(&page).editor;
    assert_eq!(editor.stems_summary, "2 · 8 kHz · WAV");
    let files: Vec<(&str, Part, bool)> = editor
        .stems
        .iter()
        .map(|row| (row.file.as_str(), row.part, row.automatic))
        .collect();
    assert_eq!(
        files,
        [
            ("keys.wav", Part::from_id("keys").unwrap(), false),
            ("click.wav", Part::from_id("click").unwrap(), false)
        ]
    );
    header(&mut page, ImportAction::TogglePartMenu(0));
    assert_eq!(view(&page).editor.part_menu.map(|menu| menu.stem), Some(0));
    let piano = Part::from_id("piano").unwrap();
    header(&mut page, ImportAction::SetPart(0, piano));
    let editor = view(&page).editor;
    assert_eq!(editor.part_menu, None, "the menu closes");
    assert_eq!(editor.stems[0].part, piano);
    save(&mut page);
    assert_eq!(bench.saved().stems[0].part, "piano");
}

#[test]
fn the_stems_panel_names_each_stem_by_its_file_name_without_its_folder() {
    // An import stores the stems as stems/<id>.wav.
    let mut package = package();
    for stem in &mut package.stems {
        stem.file = format!("stems/{}", stem.file);
    }
    let bench = Bench::with(package);
    let stems = bench.folder().join("stems");
    fs::create_dir_all(&stems).unwrap();
    wav(&stems.join("keys.wav"), level);
    wav(&stems.join("click.wav"), |_| 0.9);
    let mut page = bench.page();
    open(&mut page, "First Light");

    let editor = view(&page).editor;
    let files: Vec<&str> = editor.stems.iter().map(|row| row.file.as_str()).collect();
    assert_eq!(files, ["keys.wav", "click.wav"]);
}

#[test]
fn zoom_shows_fewer_bars_around_the_cursor_and_scrolling_moves_through_the_song() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    assert!(!view(&page).markers.can_zoom_out);
    place_cursor(&mut page, 12.0);

    song(&mut page, SongAction::ZoomIn);

    let markers = view(&page).markers;
    assert_eq!((markers.first_bar, markers.visible_bars), (9, 8));
    assert_eq!(markers.bars.len(), 8);
    assert_eq!(markers.cursor, Some(0.5));
    assert_eq!(abbreviations(&page), ["C", "Out"]);
    song(&mut page, SongAction::ZoomIn);
    song(&mut page, SongAction::ZoomIn);
    let markers = view(&page).markers;
    assert_eq!(markers.visible_bars, 4);
    assert!(!markers.can_zoom_in);
    // Four bars around the cursor's bar 13: bars 11 to 14.
    assert_eq!(markers.first_bar, 11);
    song(&mut page, SongAction::Scroll(-2_500));
    assert_eq!(view(&page).markers.first_bar, 8, "whole bars");
    song(&mut page, SongAction::Scroll(-100_000));
    assert_eq!(view(&page).markers.first_bar, 1, "it stops at the start");
    assert_eq!(
        view(&page).markers.cursor,
        None,
        "the cursor is off to the right"
    );
    song(&mut page, SongAction::Scroll(100_000));
    assert_eq!(view(&page).markers.first_bar, 13, "and at the end");
    song(&mut page, SongAction::ZoomOut);
    song(&mut page, SongAction::ZoomOut);
    assert_eq!(view(&page).markers.visible_bars, BARS);
}

#[test]
fn the_keys_type_into_the_editors_fields_only_while_one_has_the_focus() {
    let bench = Bench::new();
    let mut page = opened(&bench);
    assert!(!page.takes_keys());

    header(&mut page, ImportAction::Edit(SongField::Title));
    assert!(page.takes_keys());
    page.paste(" II");
    assert_eq!(view(&page).editor.title.text, "First Light II");
    page.stop_typing();

    assert!(!page.takes_keys());
    page.type_key(LineKey::Text("x"));
    assert_eq!(view(&page).editor.title.text, "First Light II");
}

#[test]
fn a_song_rows_add_button_still_adds_it_to_the_setlist_in_the_editor() {
    let bench = Bench::new();
    let mut page = bench.page();
    let before = page.editor().unwrap().setlist().items.len();

    page.act(LibraryAction::AddSong(0));

    assert_eq!(page.editor().unwrap().setlist().items.len(), before + 1);
    assert!(page.take_open().is_none(), "adding opens no editor");
    assert!(page.song_edit_view().is_none());
}

#[test]
fn the_import_editor_keeps_its_header_and_still_saves_one_intro_section() {
    let bench = Bench::new();
    let source = tempfile::tempdir().unwrap();
    let folder = source.path().join("New Song");
    fs::create_dir_all(&folder).unwrap();
    wav(&folder.join("Keys.wav"), level);
    let mut page = bench.page();
    let job = page.import_from(&folder);
    page.probed(job.run());
    assert!(
        page.song_edit_view().is_none(),
        "an import is no library song"
    );

    header(&mut page, ImportAction::AddChange);
    let editor = page.song_editor().expect("the import is open");
    assert_eq!(
        (
            editor.changes[0].bar.text.as_str(),
            editor.changes[0].bpm.text.as_str()
        ),
        ("9", "120"),
        "eight bars after bar 1, at the tempo in force"
    );
    header(&mut page, ImportAction::Edit(SongField::ChangeBpm(0)));
    page.type_key(LineKey::Backspace);
    page.type_key(LineKey::Text("5"));
    page.type_key(LineKey::Enter);
    header(&mut page, ImportAction::Save);
    let job = page.take_save().expect("Save song starts a save");
    page.saved(job.run());

    assert!(page.song_editor().is_none(), "saved, the Library shows");
    let json = fs::read_to_string(bench.dir.path().join("New Song.song/song.json")).unwrap();
    let saved = SongPackage::from_json(&json).unwrap();
    let sections: Vec<SectionType> = saved.sections.iter().map(|s| s.section_type).collect();
    assert_eq!(sections, [SectionType::Intro]);
    assert_eq!(
        saved.tempo_map,
        [
            TempoPoint { at: 0, bpm: 120.0 },
            TempoPoint {
                at: 8 * BAR,
                bpm: 125.0
            }
        ]
    );
}
