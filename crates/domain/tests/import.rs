//! S1, S3, S4 and S5 (F1.1–F1.5): the song an import saves, built from a
//! draft the user edited.

use domain::{
    BarBeat, BeatGrid, Bus, DEFAULT_ARRANGEMENT_ID, DraftError, DraftStem, DraftWarning, MapChange,
    Part, SectionType, SongDraft, StemRole, StoredFormat, TempoPoint, TimeSignature,
    TimeSignaturePoint, ValidationError, bus_for_stem,
};

const RATE: u32 = 48_000;

/// One minute at 48 kHz.
const MINUTE: u64 = 60 * RATE as u64;

fn stem(name: &str, stored_as: StoredFormat) -> DraftStem {
    DraftStem::new(name, stored_as, RATE, MINUTE)
}

fn draft(names: &[&str]) -> SongDraft {
    SongDraft::new(
        "Song",
        names
            .iter()
            .map(|name| stem(name, StoredFormat::Wav))
            .collect(),
    )
}

fn ts(numerator: u32, denominator: u32) -> TimeSignature {
    TimeSignature {
        numerator,
        denominator,
    }
}

fn tempo(bar: u64, bpm: f64) -> MapChange {
    MapChange {
        bar,
        bpm: Some(bpm),
        time_signature: None,
    }
}

fn meter(bar: u64, numerator: u32, denominator: u32) -> MapChange {
    MapChange {
        bar,
        bpm: None,
        time_signature: Some(ts(numerator, denominator)),
    }
}

#[track_caller]
fn errors(draft: &SongDraft) -> Vec<DraftError> {
    draft.to_package().expect_err("the draft is refused")
}

#[test]
fn a_new_draft_is_120_bpm_in_four_four_without_a_key_or_changes() {
    let draft = SongDraft::new("Folder Name", vec![stem("Drums.wav", StoredFormat::Wav)]);

    assert_eq!(draft.title, "Folder Name");
    assert_eq!(draft.bpm, 120.0);
    assert_eq!(draft.time_signature, ts(4, 4));
    assert_eq!(draft.key, None);
    assert!(draft.changes.is_empty());

    let package = draft.to_package().unwrap().package;
    assert_eq!(package.tempo_map, [TempoPoint { at: 0, bpm: 120.0 }]);
    assert_eq!(
        package.time_signature_map,
        [TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4
        }]
    );
    assert_eq!(package.key, None);
}

#[test]
fn a_stem_named_eg_1_gets_the_electric_guitar_part_and_bus_without_user_action() {
    let draft = draft(&["Song/EG 1.wav"]);

    let stem = &draft.stems()[0];
    assert_eq!(stem.part, Part::ElectricGuitar);
    assert!(stem.automatic, "the import chose it");

    let package = draft.to_package().unwrap().package;
    assert_eq!(package.stems[0].part, "electric-guitar");
    assert_eq!(package.stems[0].role, StemRole::Track);
    assert_eq!(bus_for_stem(&package.stems[0]), Bus::Guitars);
}

#[test]
fn a_stem_whose_name_names_no_instrument_is_other_on_the_default_bus() {
    let package = draft(&["Track 07.wav"]).to_package().unwrap().package;
    assert_eq!(package.stems[0].part, "other");
    assert_eq!(bus_for_stem(&package.stems[0]), Bus::KeysPads);
}

#[test]
fn a_part_the_user_picks_is_stored_by_id_with_its_role_and_is_not_automatic() {
    let mut draft = draft(&["EG 1.wav", "Track 2.wav", "Track 3.wav"]);

    assert!(draft.set_part(0, Part::Other));
    assert!(draft.set_part(1, Part::Click));
    assert!(draft.set_part(2, Part::Guide));
    assert!(!draft.set_part(3, Part::Drums), "there is no fourth stem");

    assert!(draft.stems().iter().all(|stem| !stem.automatic));
    let stems = draft.to_package().unwrap().package.stems;
    let stored: Vec<(&str, StemRole, Bus)> = stems
        .iter()
        .map(|stem| (stem.part.as_str(), stem.role, bus_for_stem(stem)))
        .collect();
    assert_eq!(
        stored,
        [
            ("other", StemRole::Track, Bus::KeysPads),
            ("click", StemRole::Click, Bus::ClickGuide),
            ("guide", StemRole::Guide, Bus::ClickGuide),
        ]
    );
}

#[test]
fn stem_ids_are_unique_slugs_of_the_file_names_and_files_are_in_the_stems_folder() {
    let draft = SongDraft::new(
        "Song",
        vec![
            stem("EG 1.wav", StoredFormat::Wav),
            stem("Take 2/EG 1.flac", StoredFormat::Flac),
            stem("EG-1.mp3", StoredFormat::Flac),
            stem("Batería Ñandú.m4a", StoredFormat::Flac),
            stem("!!!.wav", StoredFormat::Wav),
            stem("../evil.wav", StoredFormat::Wav),
        ],
    );

    let ids = draft.stem_ids();
    assert_eq!(
        ids,
        ["eg-1", "eg-1-2", "eg-1-3", "bateria-nandu", "stem", "evil"]
    );
    let files: Vec<String> = draft
        .to_package()
        .unwrap()
        .package
        .stems
        .into_iter()
        .map(|stem| stem.file)
        .collect();
    assert_eq!(
        files,
        [
            "stems/eg-1.wav",
            "stems/eg-1-2.flac",
            "stems/eg-1-3.flac",
            "stems/bateria-nandu.flac",
            "stems/stem.wav",
            "stems/evil.wav",
        ]
    );
}

#[test]
fn the_package_has_one_intro_section_and_the_default_arrangement_holding_it() {
    let mut draft = draft(&["Drums.wav", "Bass.wav"]);
    draft.title = "  Amazing Grace  ".to_owned();
    draft.key = Some(" G ".to_owned());

    let packaged = draft.to_package().unwrap();
    let package = &packaged.package;
    assert_eq!(package.title, "Amazing Grace");
    assert_eq!(package.key.as_deref(), Some("G"));
    assert_eq!(package.sample_rate, RATE);
    assert_eq!(package.sections.len(), 1);
    assert_eq!(package.sections[0].id, "intro");
    assert_eq!(package.sections[0].section_type, SectionType::Intro);
    assert_eq!(package.sections[0].start, 0);
    assert_eq!(package.arrangements.len(), 1);
    assert_eq!(package.arrangements[0].id, DEFAULT_ARRANGEMENT_ID);
    assert_eq!(package.arrangements[0].name, "Default");
    assert_eq!(package.arrangements[0].sections, ["intro"]);
    assert_eq!(packaged.song.length(), MINUTE);
    assert!(packaged.warnings.is_empty());

    draft.key = Some("   ".to_owned());
    assert_eq!(draft.to_package().unwrap().package.key, None, "no key");
}

#[test]
fn changes_at_bars_become_sample_offsets_on_the_grid_of_the_changes_before_them() {
    let mut draft = draft(&["Drums.wav"]);
    // Entered out of order; sorted by bar.
    draft.changes = vec![
        MapChange {
            bar: 9,
            bpm: Some(180.0),
            time_signature: Some(ts(6, 8)),
        },
        meter(7, 3, 4),
        tempo(5, 90.0),
    ];

    let packaged = draft.to_package().unwrap();

    // 120 BPM in 4/4: a bar is 4 × 24 000 samples, so bar 5 is at 384 000.
    // 90 BPM: a bar is 4 × 32 000, so bar 7 is two bars later at 640 000.
    // 3/4 at 90 BPM: a bar is 3 × 32 000, so bar 9 is at 832 000, where
    // 6/8 at 180 eighths per minute starts.
    assert_eq!(
        packaged.package.tempo_map,
        [
            TempoPoint { at: 0, bpm: 120.0 },
            TempoPoint {
                at: 384_000,
                bpm: 90.0
            },
            TempoPoint {
                at: 832_000,
                bpm: 180.0
            },
        ]
    );
    assert_eq!(
        packaged.package.time_signature_map,
        [
            TimeSignaturePoint {
                at: 0,
                numerator: 4,
                denominator: 4
            },
            TimeSignaturePoint {
                at: 640_000,
                numerator: 3,
                denominator: 4
            },
            TimeSignaturePoint {
                at: 832_000,
                numerator: 6,
                denominator: 8
            },
        ]
    );
    let grid = BeatGrid::new(&packaged.song);
    for (at, bar) in [(384_000, 5), (640_000, 7), (832_000, 9)] {
        assert_eq!(grid.bar_and_beat(at), BarBeat { bar, beat: 1 }, "{at}");
    }
    // 6/8 at 180: a bar is 6 × 16 000 samples.
    assert_eq!(grid.next_bar_line(832_001), Some(928_000));
}

#[test]
fn bars_restart_at_a_meter_change_and_a_fractional_bar_rounds_to_the_nearest_sample() {
    let mut draft = SongDraft::new(
        "Song",
        vec![DraftStem::new(
            "Drums.wav",
            StoredFormat::Wav,
            44_100,
            44_100 * 60,
        )],
    );
    draft.bpm = 128.0;
    draft.changes = vec![meter(2, 7, 8), meter(3, 4, 4)];

    let package = draft.to_package().unwrap().package;

    // 128 BPM at 44.1 kHz: 20 671.875 samples a beat, so bar 2 starts at
    // 82 687.5, rounded to 82 688. 7/8 counts eighths at 128 BPM, so bar 3
    // is 7 × 20 671.875 = 144 703.125 samples later, at 227 391.
    let starts: Vec<u64> = package
        .time_signature_map
        .iter()
        .map(|point| point.at)
        .collect();
    assert_eq!(starts, [0, 82_688, 227_391]);
}

#[test]
fn bad_edits_are_errors_with_clear_messages() {
    let mut draft = draft(&["Drums.wav"]);
    draft.title = "   ".to_owned();
    draft.bpm = 0.0;
    draft.time_signature = ts(0, 3);
    draft.changes = vec![
        tempo(0, 100.0),
        tempo(1, 100.0),
        tempo(4, f64::NAN),
        meter(4, 3, 4),
        MapChange {
            bar: 6,
            bpm: None,
            time_signature: None,
        },
        tempo(8, 1_000.0),
    ];

    let messages: Vec<String> = errors(&draft).iter().map(ToString::to_string).collect();

    assert_eq!(
        messages,
        [
            "the song needs a title",
            "the tempo at bar 1 is 0 BPM; it must be from 1 to 999 BPM",
            "the time signature at bar 1 is 0/3; it needs at least 1 beat per bar and a note value of 1, 2, 4, 8, 16 or 32",
            "a change is at bar 0; bars count from 1",
            "a change is at bar 1, the start of the song; set the song's tempo and time signature instead",
            "the tempo at bar 4 is NaN BPM; it must be from 1 to 999 BPM",
            "two changes are at bar 4; merge them into one",
            "the change at bar 6 sets neither a tempo nor a time signature",
            "the tempo at bar 8 is 1000 BPM; it must be from 1 to 999 BPM",
        ]
    );
}

#[test]
fn a_change_past_the_last_bar_is_an_error_that_names_the_last_bar() {
    // One minute at 120 BPM in 4/4 is 30 bars.
    let mut draft = draft(&["Drums.wav"]);
    draft.changes = vec![tempo(30, 100.0)];
    assert!(draft.to_package().is_ok(), "bar 30 starts before the end");

    draft.changes = vec![tempo(31, 100.0)];
    assert_eq!(
        errors(&draft),
        [DraftError::ChangePastEnd {
            bar: 31,
            last_bar: 30
        }]
    );
    assert_eq!(
        errors(&draft)[0].to_string(),
        "the change at bar 31 is past the end of the song, whose last bar is bar 30"
    );
}

#[test]
fn a_draft_without_stems_is_refused() {
    let draft = SongDraft::new("Song", Vec::new());
    assert_eq!(errors(&draft), [DraftError::NoStems]);
}

#[test]
fn a_stem_at_another_sample_rate_blocks_saving_and_names_its_file() {
    let draft = SongDraft::new(
        "Song",
        vec![
            DraftStem::new("Click.wav", StoredFormat::Wav, 44_100, MINUTE),
            stem("Drums.wav", StoredFormat::Wav),
            stem("Bass.wav", StoredFormat::Wav),
        ],
    );

    assert_eq!(draft.sample_rate(), Some(RATE), "the rate most stems share");
    let errors = errors(&draft);
    assert_eq!(
        errors,
        [DraftError::SampleRateMismatch {
            file: "Click.wav".to_owned(),
            found: 44_100,
            expected: RATE,
        }]
    );
    assert_eq!(
        errors[0].to_string(),
        "\"Click.wav\" is 44100 Hz, but the song is 48000 Hz; every stem must have the same sample rate"
    );
}

#[test]
fn stem_lengths_more_than_a_second_apart_warn_without_blocking() {
    let draft = SongDraft::new(
        "Song",
        vec![
            stem("Drums.wav", StoredFormat::Wav),
            DraftStem::new(
                "Pad.wav",
                StoredFormat::Wav,
                RATE,
                MINUTE - u64::from(RATE) - 1,
            ),
        ],
    );

    let packaged = draft.to_package().expect("a warning does not block");

    assert_eq!(
        packaged.warnings,
        [DraftWarning::StemLengthsDiffer {
            shortest: "Pad.wav".to_owned(),
            shortest_length: MINUTE - 48_001,
            longest: "Drums.wav".to_owned(),
            longest_length: MINUTE,
        }]
    );
    assert_eq!(
        packaged.warnings[0].to_string(),
        "stem lengths differ by more than one second: \"Pad.wav\" has 2831999 frames, \"Drums.wav\" has 2880000"
    );
}

#[test]
fn two_click_stems_block_saving_and_name_both_files() {
    let draft = draft(&["Click.wav", "Take 2/Click.wav", "Drums.wav"]);

    let errors = errors(&draft);

    assert_eq!(
        errors,
        [DraftError::Package {
            error: ValidationError::TooManyStemsWithRole {
                role: StemRole::Click,
                ids: vec!["click".to_owned(), "click-2".to_owned()],
            },
            files: vec!["Click.wav".to_owned(), "Take 2/Click.wav".to_owned()],
        }]
    );
    assert_eq!(
        errors[0].to_string(),
        "only one click stem is allowed, found \"click\", \"click-2\" (\"Click.wav\", \"Take 2/Click.wav\")"
    );
}
