//! S7 (F1): the Library screen's "Import stems" and the Song editor without
//! a window, on a temporary library folder and a temporary folder of WAV
//! stems. The probe and the save run as the jobs the screen hands to its
//! background executor; here they run on the test's thread, and what they
//! write is read back from disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use domain::{
    Arrangement, Bus, DraftStem, Part, SectionMarker, SectionType, SongDraft, SongPackage, Stem,
    StemRole, StoredFormat, TempoPoint, TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use openvamp_desktop::audio_settings::AudioSettings;
use openvamp_desktop::hold::PanicHold;
use openvamp_desktop::library_page::{LibraryAction, LibraryPage, LibraryView};
use openvamp_desktop::line_edit::LineKey;
use openvamp_desktop::mixer::MixerPage;
use openvamp_desktop::output_page::OutputPage;
use openvamp_desktop::song_import::{
    ChangeRow, ImportAction, SongEditorView, SongField, StemRow, stems_summary,
};
use openvamp_desktop::tick::Timed;

const RATE: u32 = 8_000;

/// A WAV file of `seconds` of silence at `rate`, with `channels` channels of
/// `bits` bits.
fn wav(path: &Path, rate: u32, seconds: f64, channels: u16, bits: u16) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let spec = WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).unwrap();
    let frames = (seconds * f64::from(rate)) as usize;
    for _ in 0..frames * usize::from(channels) {
        if bits == 8 {
            writer.write_sample(0i8).unwrap();
        } else {
            writer.write_sample(0i16).unwrap();
        }
    }
    writer.finalize().unwrap();
}

/// A mono 16-bit stem of `seconds` at [`RATE`].
fn stem(path: &Path, seconds: f64) {
    wav(path, RATE, seconds, 1, 16);
}

/// A song folder "Amazing" with one stem, as the library already holds.
fn write_song(dir: &Path) {
    let package = SongPackage {
        format_version: 1,
        title: "Amazing".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: Some("A".to_owned()),
        tempo_map: vec![TempoPoint { at: 0, bpm: 132.0 }],
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
        sections: vec![SectionMarker {
            id: "intro".to_owned(),
            section_type: SectionType::Intro,
            number: None,
            start: 0,
        }],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["intro".to_owned()],
            infinite_loop: Vec::new(),
        }],
    };
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    stem(&dir.join("keys.wav"), 1.0);
}

/// A temporary folder with a library folder holding "Amazing" and a
/// source folder "First Light" with the stems `files`, each of `seconds`.
struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new(files: &[&str], seconds: f64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_song(&dir.path().join("Library/Amazing.song"));
        for file in files {
            stem(&dir.path().join("First Light").join(file), seconds);
        }
        Self { dir }
    }

    fn library(&self) -> PathBuf {
        self.dir.path().join("Library")
    }

    fn source(&self) -> PathBuf {
        self.dir.path().join("First Light")
    }

    fn page(&self) -> LibraryPage {
        LibraryPage::open(&self.library(), None).unwrap()
    }

    /// The library folder's entries, hidden ones too, by name.
    fn library_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.library())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

fn act(page: &mut LibraryPage, action: ImportAction) {
    page.act(LibraryAction::Import(action));
}

/// Imports `source` as the screen does: the probe runs, then the page
/// takes what it read.
fn import(page: &mut LibraryPage, source: &Path) {
    let job = page.import_from(source);
    page.probed(job.run());
}

fn editor(page: &LibraryPage) -> SongEditorView {
    page.song_editor().expect("the Song editor is open")
}

fn library_view(page: &LibraryPage) -> LibraryView {
    page.view(None)
}

/// "Save song", and the save run as the screen runs it.
fn save(page: &mut LibraryPage) {
    act(page, ImportAction::Save);
    let job = page.take_save().expect("Save song starts a save");
    page.saved(job.run());
}

/// Clicks `field` and types `text` in place of what it holds, then Enter.
fn type_into(page: &mut LibraryPage, field: SongField, text: &str) {
    act(page, ImportAction::Edit(field));
    let old = field_text(&editor(page), field);
    for _ in old.chars() {
        page.type_key(LineKey::Backspace);
    }
    page.type_key(LineKey::Text(text));
    page.type_key(LineKey::Enter);
}

fn field_text(view: &SongEditorView, field: SongField) -> String {
    let change = |index: usize| -> &ChangeRow { &view.changes[index] };
    match field {
        SongField::Title => view.title.text.clone(),
        SongField::Key => view.key.text.clone(),
        SongField::Tempo => view.tempo.text.clone(),
        SongField::Time => view.time.text.clone(),
        SongField::ChangeBar(index) => change(index).bar.text.clone(),
        SongField::ChangeBpm(index) => change(index).bpm.text.clone(),
        SongField::ChangeTime(index) => change(index).time.text.clone(),
    }
}

fn row<'a>(view: &'a SongEditorView, file: &str) -> &'a StemRow {
    view.stems
        .iter()
        .find(|row| row.file == file)
        .unwrap_or_else(|| panic!("{file} is listed: {:?}", view.stems))
}

/// The package saved in the library folder as `folder`.
fn saved_package(bench: &Bench, folder: &str) -> SongPackage {
    let json = fs::read_to_string(bench.library().join(folder).join("song.json")).unwrap();
    SongPackage::from_json(&json).unwrap()
}

#[test]
fn the_import_button_reads_the_source_off_the_ui_thread_and_opens_the_song_editor() {
    let bench = Bench::new(&["Kick.wav", "EG 1.wav"], 1.0);
    let mut page = bench.page();

    let job = page.import_from(&bench.source());

    let busy = library_view(&page).importing;
    assert_eq!(busy.as_deref(), Some("Reading First Light…"));
    assert!(
        page.song_editor().is_none(),
        "nothing opens before the read"
    );
    page.probed(job.run());
    let view = editor(&page);
    assert_eq!(library_view(&page).importing, None);
    assert_eq!(view.title.text, "First Light");
    assert_eq!(view.tempo.text, "120");
    assert_eq!(view.time.text, "4/4");
    assert_eq!(view.key.text, "");
    assert_eq!(
        view.source,
        bench.source().canonicalize().unwrap().display().to_string()
    );
    assert!(view.can_save, "{:?}", view.problems);
}

#[test]
fn only_the_newest_import_opens() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let other = bench.dir.path().join("Other Song");
    stem(&other.join("Bass.wav"), 1.0);
    let mut page = bench.page();

    let first = page.import_from(&bench.source());
    let second = page.import_from(&other);
    page.probed(first.run());

    assert!(page.song_editor().is_none(), "a pick replaced by another");
    assert_eq!(
        library_view(&page).importing.as_deref(),
        Some("Reading Other Song…")
    );
    page.probed(second.run());
    assert_eq!(editor(&page).title.text, "Other Song");
}

#[test]
fn the_song_editor_lists_each_stem_with_its_icon_auto_badge_and_bus() {
    let bench = Bench::new(&["EG 1.wav", "Kick.wav", "Bass DI.wav", "Mystery.wav"], 1.0);
    let mut page = bench.page();

    import(&mut page, &bench.source());

    let view = editor(&page);
    let eg = row(&view, "EG 1.wav");
    assert_eq!(
        (eg.part, eg.bus, eg.automatic, eg.icon),
        (Part::ElectricGuitar, Bus::Guitars, true, "electric-guitar")
    );
    let kick = row(&view, "Kick.wav");
    assert_eq!((kick.bus, kick.icon), (Bus::Drums, "drum"));
    let bass = row(&view, "Bass DI.wav");
    assert_eq!((bass.bus, bass.icon), (Bus::Bass, "bass"));
    let mystery = row(&view, "Mystery.wav");
    assert_eq!(
        (mystery.part, mystery.bus, mystery.icon),
        (Part::Other, Bus::KeysPads, "note")
    );
    assert!(view.stems.iter().all(|row| !row.menu_open));
    assert_eq!(view.part_menu, None);
}

#[test]
fn the_stems_header_gives_the_count_the_rate_in_khz_and_the_stored_format() {
    let bench = Bench::new(&["Kick.wav", "Snare.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    assert_eq!(editor(&page).stems_summary, "2 · 8 kHz · WAV");

    // An 8-bit WAV is stored as FLAC: both formats.
    wav(&bench.source().join("Shaker.wav"), RATE, 1.0, 1, 8);
    import(&mut page, &bench.source());
    assert_eq!(editor(&page).stems_summary, "3 · 8 kHz · WAV + FLAC");

    let flac = |rate| DraftStem::new("Pad.flac", StoredFormat::Flac, rate, 1_000);
    let draft = SongDraft::new("Pads", vec![flac(48_000); 16]);
    assert_eq!(stems_summary(&draft), "16 · 48 kHz · FLAC");
    let draft = SongDraft::new("Pads", vec![flac(44_100), flac(44_100), flac(48_000)]);
    assert_eq!(stems_summary(&draft), "3 · 44.1 kHz · FLAC");
    let draft = SongDraft::new("Pads", vec![flac(11_025)]);
    assert_eq!(stems_summary(&draft), "1 · 11.025 kHz · FLAC");
}

#[test]
fn choosing_a_part_clears_auto_and_moves_the_stem_to_that_part_s_bus() {
    let bench = Bench::new(&["EG 1.wav", "Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    let index = row(&editor(&page), "EG 1.wav").index;

    act(&mut page, ImportAction::TogglePartMenu(index));

    let view = editor(&page);
    assert!(row(&view, "EG 1.wav").menu_open);
    let menu = view.part_menu.expect("the part menu is open");
    assert_eq!(menu.stem, index);
    let buses: Vec<Bus> = menu.groups.iter().map(|group| group.bus).collect();
    assert_eq!(buses, Bus::ALL);
    let parts: Vec<Part> = menu
        .groups
        .iter()
        .flat_map(|group| group.parts.iter().map(|choice| choice.part))
        .collect();
    assert_eq!(parts, Part::ALL);
    for group in &menu.groups {
        assert!(
            group
                .parts
                .iter()
                .all(|choice| choice.part.bus() == group.bus)
        );
    }
    let selected: Vec<Part> = menu
        .groups
        .iter()
        .flat_map(|group| group.parts.iter())
        .filter(|choice| choice.selected)
        .map(|choice| choice.part)
        .collect();
    assert_eq!(selected, [Part::ElectricGuitar]);

    act(&mut page, ImportAction::SetPart(index, Part::Vocals));

    let view = editor(&page);
    let eg = row(&view, "EG 1.wav");
    assert_eq!(
        (eg.part, eg.bus, eg.automatic, eg.menu_open),
        (Part::Vocals, Bus::Vocals, false, false)
    );
    assert_eq!(view.part_menu, None);
    assert!(row(&view, "Kick.wav").automatic, "the other stem stays");

    save(&mut page);
    let package = saved_package(&bench, "First Light.song");
    let parts: Vec<&str> = package.stems.iter().map(|s| s.part.as_str()).collect();
    assert_eq!(parts, ["vocals", "drums"]);
}

#[test]
fn the_part_menu_closes_again_or_on_another_control() {
    let bench = Bench::new(&["EG 1.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::TogglePartMenu(0));
    act(&mut page, ImportAction::TogglePartMenu(0));
    assert_eq!(editor(&page).part_menu, None);
    act(&mut page, ImportAction::TogglePartMenu(0));
    act(&mut page, ImportAction::ClosePartMenu);
    assert_eq!(editor(&page).part_menu, None);
    act(&mut page, ImportAction::TogglePartMenu(0));
    act(&mut page, ImportAction::Edit(SongField::Key));
    assert_eq!(editor(&page).part_menu, None);
    assert!(row(&editor(&page), "EG 1.wav").automatic);
}

#[test]
fn title_key_tempo_and_time_edits_reach_the_saved_song() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    type_into(&mut page, SongField::Title, "Morning Light");
    type_into(&mut page, SongField::Key, "G");
    type_into(&mut page, SongField::Tempo, "104.5");
    type_into(&mut page, SongField::Time, "6/8");

    let view = editor(&page);
    assert_eq!(view.title.text, "Morning Light");
    assert_eq!(view.key.text, "G");
    assert_eq!(view.tempo.text, "104.5");
    assert_eq!(view.time.text, "6/8");
    assert!(!view.title.editing && !view.time.editing);
    save(&mut page);
    let package = saved_package(&bench, "Morning Light.song");
    assert_eq!(package.title, "Morning Light");
    assert_eq!(package.key.as_deref(), Some("G"));
    assert_eq!(package.tempo_map, [TempoPoint { at: 0, bpm: 104.5 }]);
    assert_eq!(
        package.time_signature_map,
        [TimeSignaturePoint {
            at: 0,
            numerator: 6,
            denominator: 8
        }]
    );
}

#[test]
fn a_field_shows_its_caret_while_typing_and_escape_puts_its_text_back() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::Edit(SongField::Tempo));
    assert!(page.takes_keys(), "the keys type into the field");
    assert!(editor(&page).tempo.editing);
    page.type_key(LineKey::Text("5"));
    assert_eq!(editor(&page).tempo.text, "1205");
    page.type_key(LineKey::Escape);

    assert!(!page.takes_keys());
    let view = editor(&page);
    assert_eq!(
        (view.tempo.text.as_str(), view.tempo.editing),
        ("120", false)
    );
    assert!(view.can_save);
}

#[test]
fn text_that_is_no_tempo_or_time_signature_blocks_save_and_says_why() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    type_into(&mut page, SongField::Tempo, "fast");
    type_into(&mut page, SongField::Time, "4-4");
    type_into(&mut page, SongField::Title, " ");

    let view = editor(&page);
    assert!(!view.can_save);
    assert_eq!(
        view.problems,
        [
            "The tempo \"fast\" is not a number of beats per minute, such as 120.",
            "The time signature \"4-4\" is not written like 4/4.",
            "The song needs a title.",
        ]
    );
    act(&mut page, ImportAction::Save);
    assert!(page.take_save().is_none(), "Save song saves nothing");
}

#[test]
fn a_tempo_or_meter_change_is_added_at_a_bar_and_saved() {
    // 20 s at 120 BPM in 4/4: ten bars.
    let bench = Bench::new(&["Kick.wav"], 20.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::AddChange);

    let view = editor(&page);
    assert_eq!(view.changes.len(), 1);
    assert_eq!(view.changes[0].bar.text, "9");
    assert_eq!(view.changes[0].bpm.text, "120");
    assert_eq!(view.changes[0].time.text, "");
    type_into(&mut page, SongField::ChangeBar(0), "5");
    type_into(&mut page, SongField::ChangeBpm(0), "90");
    type_into(&mut page, SongField::ChangeTime(0), "3/4");
    assert!(editor(&page).can_save, "{:?}", editor(&page).problems);
    save(&mut page);

    let package = saved_package(&bench, "First Light.song");
    // Bar 5 starts after four bars of 4/4 at 120 BPM: 8 s.
    let at = 8 * u64::from(RATE);
    assert_eq!(
        package.tempo_map,
        [
            TempoPoint { at: 0, bpm: 120.0 },
            TempoPoint { at, bpm: 90.0 }
        ]
    );
    assert_eq!(package.time_signature_map[1].at, at);
    assert_eq!(package.time_signature_map[1].numerator, 3);
}

#[test]
fn a_change_with_its_tempo_left_empty_changes_only_the_meter_and_one_can_be_removed() {
    let bench = Bench::new(&["Kick.wav"], 20.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::AddChange);
    act(&mut page, ImportAction::AddChange);
    assert_eq!(editor(&page).changes[1].bar.text, "17", "eight bars on");
    act(&mut page, ImportAction::RemoveChange(1));
    assert_eq!(editor(&page).changes.len(), 1);
    type_into(&mut page, SongField::ChangeBpm(0), "");
    type_into(&mut page, SongField::ChangeTime(0), "7/8");
    save(&mut page);

    let package = saved_package(&bench, "First Light.song");
    assert_eq!(package.tempo_map, [TempoPoint { at: 0, bpm: 120.0 }]);
    assert_eq!(package.time_signature_map.len(), 2);
    assert_eq!(package.time_signature_map[1].numerator, 7);
}

#[test]
fn a_change_removed_is_not_saved() {
    let bench = Bench::new(&["Kick.wav"], 20.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::AddChange);
    act(&mut page, ImportAction::RemoveChange(0));

    assert!(editor(&page).changes.is_empty());
    save(&mut page);
    let package = saved_package(&bench, "First Light.song");
    assert_eq!(package.tempo_map.len(), 1);
    assert_eq!(package.time_signature_map.len(), 1);
}

#[test]
fn a_change_the_domain_refuses_blocks_save_with_its_message() {
    // 1 s at 120 BPM: only bar 1.
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::AddChange);

    let view = editor(&page);
    assert!(!view.can_save);
    assert_eq!(
        view.problems,
        ["The change at bar 9 is past the end of the song, whose last bar is bar 1."]
    );
}

#[test]
fn a_sample_rate_mismatch_disables_save_and_names_the_file() {
    let bench = Bench::new(&["Kick.wav", "Snare.wav"], 1.0);
    wav(&bench.source().join("Pad.wav"), 16_000, 1.0, 1, 16);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    let view = editor(&page);

    assert!(!view.can_save);
    assert_eq!(
        view.problems,
        [
            "\"Pad.wav\" is 16000 Hz, but the song is 8000 Hz; every stem must have the same sample rate."
        ]
    );
    act(&mut page, ImportAction::Save);
    assert!(page.take_save().is_none());
    assert_eq!(bench.library_entries(), ["Amazing.song"]);
}

#[test]
fn stem_lengths_more_than_a_second_apart_warn_and_save_stays_allowed() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    stem(&bench.source().join("Pad.wav"), 3.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    let view = editor(&page);

    assert!(view.can_save, "{:?}", view.problems);
    assert_eq!(view.warnings.len(), 1);
    assert!(
        view.warnings[0].starts_with("Stem lengths differ by more than one second"),
        "{}",
        view.warnings[0]
    );
    save(&mut page);
    assert!(page.song_editor().is_none(), "saved");
}

#[test]
fn files_the_import_refuses_are_listed_with_their_reasons_and_block_save() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    wav(&bench.source().join("Surround.wav"), RATE, 0.1, 3, 16);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    let view = editor(&page);

    assert_eq!(view.stems.len(), 1);
    assert_eq!(view.refused.len(), 1);
    assert_eq!(view.refused[0].file, "Surround.wav");
    assert_eq!(
        view.refused[0].reason,
        "it has 3 channels; stems must be mono or stereo"
    );
    assert!(!view.can_save);
    assert_eq!(
        view.problems,
        [
            "1 file cannot be imported, so the song cannot be saved: take it out of the source and import again."
        ]
    );
    act(&mut page, ImportAction::Save);
    assert!(page.take_save().is_none());
}

#[test]
fn save_writes_the_song_and_the_library_lists_and_marks_it() {
    let bench = Bench::new(&["Kick.wav", "EG 1.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());

    act(&mut page, ImportAction::Save);
    let job = page.take_save().expect("a save to run");
    let saving = editor(&page).saving.expect("the save shows");
    assert_eq!(
        (
            saving.label.as_str(),
            saving.cancelling,
            editor(&page).can_save
        ),
        ("Saving… 0%", false, false)
    );
    page.saved(job.run());

    assert!(page.song_editor().is_none(), "back on the Library");
    assert_eq!(
        bench.library_entries(),
        ["Amazing.song", "First Light.song"],
        "nothing hidden is left"
    );
    let view = library_view(&page);
    let titles: Vec<(&str, bool)> = view
        .songs
        .iter()
        .map(|song| (song.title.as_str(), song.imported))
        .collect();
    assert_eq!(titles, [("Amazing", false), ("First Light", true)]);
    assert_eq!(view.songs[1].stems, "2 stems");
    assert_eq!(page.imported_row(), Some(1));
    let package = saved_package(&bench, "First Light.song");
    assert_eq!(package.stems.len(), 2);
}

#[test]
fn an_imported_song_hidden_by_the_search_clears_the_search() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    page.act(LibraryAction::Search);
    page.type_key(LineKey::Text("Ama"));
    page.type_key(LineKey::Enter);
    import(&mut page, &bench.source());

    save(&mut page);

    let view = library_view(&page);
    assert_eq!(view.search, "");
    assert_eq!(view.songs.len(), 2);
}

#[test]
fn the_imported_song_can_be_added_to_a_setlist_at_once() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    page.act(LibraryAction::NewSetlist);
    page.type_key(LineKey::Enter);
    import(&mut page, &bench.source());
    save(&mut page);

    let row = page.imported_row().unwrap();
    page.act(LibraryAction::AddSong(row));

    let view = library_view(&page);
    let editor = view.editor.expect("the setlist stays in the editor");
    let titles: Vec<&str> = editor.rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(titles, ["First Light"]);
    assert_eq!(editor.rows[0].length, "0:01");
    assert!(
        !library_view(&page).songs.iter().any(|song| song.imported),
        "the mark goes with the next control"
    );
}

#[test]
fn a_save_that_fails_keeps_the_editor_open_with_the_reason() {
    let bench = Bench::new(&["Kick.wav", "Snare.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    fs::remove_file(bench.source().join("Snare.wav")).unwrap();

    save(&mut page);

    let view = editor(&page);
    let problem = view.save_problem.expect("the reason shows");
    assert!(
        problem.starts_with("The song could not be saved: "),
        "{problem}"
    );
    assert!(problem.contains("Snare.wav"), "{problem}");
    assert!(view.saving.is_none());
    assert!(view.can_save, "it can be tried again");
    assert_eq!(bench.library_entries(), ["Amazing.song"]);
}

#[test]
fn discard_before_saving_drops_the_draft_and_writes_nothing() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    type_into(&mut page, SongField::Title, "Changed");

    act(&mut page, ImportAction::Discard);

    assert!(page.song_editor().is_none());
    assert!(page.take_save().is_none());
    assert_eq!(bench.library_entries(), ["Amazing.song"]);
    let titles: Vec<String> = library_view(&page)
        .songs
        .into_iter()
        .map(|song| song.title)
        .collect();
    assert_eq!(titles, ["Amazing"]);
}

#[test]
fn discard_during_the_save_cancels_it_and_leaves_nothing_behind() {
    let bench = Bench::new(&["Kick.wav", "Snare.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    act(&mut page, ImportAction::Save);
    let job = page.take_save().unwrap();

    act(&mut page, ImportAction::Discard);

    let view = editor(&page);
    let saving = view.saving.expect("the editor waits for the save to stop");
    assert_eq!(
        (saving.label.as_str(), saving.cancelling),
        ("Cancelling…", true)
    );
    act(&mut page, ImportAction::Save);
    assert!(page.take_save().is_none(), "no second save while one stops");
    page.saved(job.run());
    assert!(page.song_editor().is_none(), "back on the Library");
    assert_eq!(page.notice(), None);
    assert_eq!(bench.library_entries(), ["Amazing.song"]);
}

#[test]
fn a_save_that_ends_before_discard_takes_effect_is_listed_and_says_so() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    act(&mut page, ImportAction::Save);
    let done = page.take_save().unwrap().run();

    act(&mut page, ImportAction::Discard);
    page.saved(done);

    assert!(page.song_editor().is_none());
    let view = library_view(&page);
    assert_eq!(
        view.warning.as_deref(),
        Some("The import had already finished: \"First Light\" was saved.")
    );
    assert!(view.songs.iter().any(|song| song.title == "First Light"));
}

#[test]
fn a_source_that_cannot_be_imported_says_why_on_the_library_screen() {
    let bench = Bench::new(&[], 1.0);
    let empty = bench.dir.path().join("Empty");
    fs::create_dir(&empty).unwrap();
    let text = bench.dir.path().join("notes.txt");
    fs::write(&text, "no stems").unwrap();
    let zip = bench.dir.path().join("Broken.zip");
    fs::write(&zip, "not a zip at all").unwrap();
    let mut page = bench.page();

    for (source, says) in [
        (&empty, "holds no WAV, FLAC, MP3 or M4A file"),
        (&text, "is neither a folder nor a .zip file"),
        (&zip, "is not a readable ZIP file"),
    ] {
        import(&mut page, source);

        assert!(page.song_editor().is_none());
        let view = library_view(&page);
        assert_eq!(view.importing, None);
        let notice = view.notice.expect("the reason shows");
        assert!(notice.starts_with("Cannot import stems: "), "{notice}");
        assert!(notice.contains(says), "{notice}");
        page.act(LibraryAction::DismissNotice);
    }
    assert_eq!(bench.library_entries(), ["Amazing.song"]);
}

#[test]
fn the_screen_draws_again_while_the_save_progress_moves() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    let (mut hold, mut mixer, mut output) = (PanicHold::new(), MixerPage::new(), OutputPage::new());
    let mut settings = AudioSettings::load(None);
    let mut turn = |page: &mut LibraryPage| {
        Timed {
            show: None,
            hold: &mut hold,
            mixer: &mut mixer,
            output: &mut output,
            settings: &mut settings,
            library: Some(page),
        }
        .turn(Instant::now())
    };
    assert!(!turn(&mut page), "nothing moves before a save");
    act(&mut page, ImportAction::Save);
    let job = page.take_save().unwrap();
    turn(&mut page);
    assert!(!turn(&mut page), "no progress yet");

    let done = job.run();

    assert!(turn(&mut page), "the progress moved");
    assert!(!turn(&mut page), "and stands still");
    assert_eq!(editor(&page).saving.unwrap().label, "Saving… 100%");
    page.saved(done);
    assert!(!turn(&mut page));
}

#[test]
fn every_stem_shows_once_asked_for() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    assert!(!editor(&page).all_stems);

    act(&mut page, ImportAction::ShowAllStems);

    assert!(editor(&page).all_stems);
}

#[test]
fn while_the_song_editor_is_open_the_library_controls_and_keys_still_answer_after_it() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let mut page = bench.page();
    page.act(LibraryAction::Search);
    page.type_key(LineKey::Text("zz"));
    import(&mut page, &bench.source());

    assert!(
        !page.takes_keys(),
        "opening the editor ends typing in the search"
    );
    act(&mut page, ImportAction::Discard);
    assert_eq!(
        library_view(&page).search,
        "zz",
        "the search keeps its text"
    );
    page.act(LibraryAction::Search);
    assert!(page.takes_keys());
    page.type_key(LineKey::Backspace);
    page.type_key(LineKey::Backspace);
    page.type_key(LineKey::Text("Ama"));
    let titles: Vec<String> = library_view(&page)
        .songs
        .into_iter()
        .map(|song| song.title)
        .collect();
    assert_eq!(titles, ["Amazing"]);
}

#[test]
fn removing_a_change_keeps_what_the_changes_after_it_read_as() {
    let bench = Bench::new(&["Kick.wav"], 60.0);
    let mut page = bench.page();
    import(&mut page, &bench.source());
    for _ in 0..3 {
        act(&mut page, ImportAction::AddChange);
    }
    type_into(&mut page, SongField::ChangeBar(2), "x");

    act(&mut page, ImportAction::RemoveChange(0));

    let view = editor(&page);
    let bars: Vec<&str> = view.changes.iter().map(|c| c.bar.text.as_str()).collect();
    assert_eq!(bars, ["17", "x"]);
    assert_eq!(
        view.problems,
        ["The bar \"x\" of a change is not a whole number, such as 17."]
    );
    type_into(&mut page, SongField::ChangeBar(1), "21");
    assert!(editor(&page).can_save, "{:?}", editor(&page).problems);
}

#[test]
fn a_save_whose_library_page_was_replaced_stops_or_stays_in_its_own_library() {
    let bench = Bench::new(&["Kick.wav"], 1.0);
    let other = Bench::new(&[], 1.0);
    fs::rename(
        other.library().join("Amazing.song"),
        other.library().join("Other.song"),
    )
    .unwrap();

    // The page goes before the save runs: the save stops.
    let mut page = bench.page();
    import(&mut page, &bench.source());
    act(&mut page, ImportAction::Save);
    let job = page.take_save().unwrap();
    drop(page);
    let mut replaced = other.page();
    replaced.saved(job.run());
    assert_eq!(
        bench.library_entries(),
        ["Amazing.song"],
        "nothing left behind"
    );

    // The save ends first: its song stays in its library, and the page of
    // another library lists only its own songs.
    let mut page = bench.page();
    import(&mut page, &bench.source());
    act(&mut page, ImportAction::Save);
    let done = page.take_save().unwrap().run();
    drop(page);
    replaced.saved(done);
    assert_eq!(
        bench.library_entries(),
        ["Amazing.song", "First Light.song"]
    );
    let paths: Vec<String> = library_view(&replaced)
        .songs
        .into_iter()
        .map(|song| song.path)
        .collect();
    assert_eq!(paths, ["Other.song"]);
}
