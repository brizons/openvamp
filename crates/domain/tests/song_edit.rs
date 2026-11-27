//! S1 and S2 (F2.1–F2.4): editing a song's section markers and arrangements.
//! Every edit leaves a package `validate` accepts, or leaves it unchanged and
//! says why.

mod common;

use std::collections::HashSet;
use std::ops::Range;

use common::{RATE, SongSpec, four_section_spec, info, marker};
use domain::{
    Arrangement, ArrangementSummary, BarBeat, CompileOptions, MAX_SONG_HOURS, RenderPlan,
    SectionType, Snap, Song, SongEditError, SongEditor, SongPackage, ValidationError, compile,
    validate,
};

/// 120 bpm in 4/4 at 48 kHz.
const BAR: u64 = 96_000;
const BEAT: u64 = 24_000;
const LENGTH: u64 = 12 * BAR;

/// Intro (bars 1–2), Verse 1 (3–6), Chorus (7–10), Outro (11–12); the
/// Default arrangement plays them in order and "live" plays the chorus three
/// times and no verse.
fn package() -> SongPackage {
    four_section_spec()
        .arrangement("live", &["intro", "chorus", "chorus", "chorus", "outro"])
        .package()
}

fn editor_of(package: SongPackage) -> SongEditor {
    SongEditor::new(package, LENGTH).expect("the package is valid")
}

fn editor() -> SongEditor {
    editor_of(package())
}

/// The editor's package as the player loads it, against its real stem.
#[track_caller]
fn song(editor: &SongEditor) -> Song {
    validate(editor.package(), &[info("drums", RATE, LENGTH)])
        .expect("the edited package is valid")
        .song
}

#[track_caller]
fn plan(editor: &SongEditor, arrangement: &str) -> RenderPlan {
    compile(&song(editor), arrangement, &CompileOptions::default()).expect("it compiles")
}

/// Each compiled position as (section, source start, length).
fn positions(editor: &SongEditor, arrangement: &str) -> Vec<(String, u64, u64)> {
    plan(editor, arrangement)
        .segments
        .into_iter()
        .map(|segment| (segment.section_id, segment.src_start, segment.len))
        .collect()
}

/// What an arrangement sounds like: the source ranges it plays, contiguous
/// ones merged, each with the crossfade it starts with.
fn audio(plan: &RenderPlan) -> Vec<(Range<u64>, u64)> {
    let mut audio: Vec<(Range<u64>, u64)> = Vec::new();
    for segment in &plan.segments {
        let range = segment.src_start..segment.src_start + segment.len;
        match audio.last_mut() {
            Some((last, _)) if last.end == range.start && segment.fade_in == 0 => {
                last.end = range.end;
            }
            _ => audio.push((range, segment.fade_in)),
        }
    }
    audio
}

fn ids(editor: &SongEditor) -> Vec<&str> {
    editor
        .package()
        .sections
        .iter()
        .map(|section| section.id.as_str())
        .collect()
}

fn arrangement<'a>(editor: &'a SongEditor, id: &str) -> &'a Arrangement {
    editor
        .package()
        .arrangements
        .iter()
        .find(|arrangement| arrangement.id == id)
        .expect("the arrangement exists")
}

fn order<'a>(editor: &'a SongEditor, id: &str) -> Vec<&'a str> {
    arrangement(editor, id)
        .sections
        .iter()
        .map(String::as_str)
        .collect()
}

/// Runs an edit that must be refused and checks it left the package as it
/// was.
#[track_caller]
fn refused<T: std::fmt::Debug>(
    editor: &mut SongEditor,
    edit: impl FnOnce(&mut SongEditor) -> Result<T, SongEditError>,
) -> SongEditError {
    let before = editor.package().clone();
    let error = edit(editor).expect_err("the edit is refused");
    assert_eq!(
        editor.package(),
        &before,
        "a refused edit leaves the package unchanged"
    );
    error
}

#[test]
fn section_type_all_is_the_built_in_list_in_declared_order() {
    use SectionType::*;
    assert_eq!(
        SectionType::ALL,
        [
            CountOff,
            Intro,
            Verse,
            PreChorus,
            Chorus,
            PostChorus,
            Bridge,
            Instrumental,
            Interlude,
            Tag,
            Vamp,
            Turnaround,
            Breakdown,
            Outro,
            Ending,
        ]
    );
    let names: HashSet<&str> = SectionType::ALL.iter().map(|t| t.name()).collect();
    assert_eq!(names.len(), 15, "every type is listed once");
}

#[test]
fn the_editor_starts_from_a_valid_package_and_gives_it_back() {
    let mut broken = package();
    broken.sections[1].start = 0;
    let errors = SongEditor::new(broken, LENGTH).expect_err("the package is invalid");
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, ValidationError::SectionStartsNotIncreasing { .. })),
        "{errors:?}"
    );

    let editor = editor();
    assert_eq!(editor.length_frames(), LENGTH);
    assert_eq!(editor.into_package(), package());
}

#[test]
fn snap_to_bar_picks_the_nearest_bar_line_and_without_it_the_nearest_beat() {
    let editor = editor();

    assert_eq!(editor.snap(5 * BAR + 47_999, Snap::Bar), 5 * BAR);
    assert_eq!(editor.snap(5 * BAR + 48_001, Snap::Bar), 6 * BAR);
    assert_eq!(
        editor.snap(5 * BAR + 48_000, Snap::Bar),
        5 * BAR,
        "a tie goes to the earlier line"
    );
    assert_eq!(editor.snap(5 * BAR + 11_999, Snap::Beat), 5 * BAR);
    assert_eq!(editor.snap(5 * BAR + 12_001, Snap::Beat), 5 * BAR + BEAT);
    assert_eq!(editor.snap(5 * BAR + 12_000, Snap::Beat), 5 * BAR);
    // Lines start before the song end, so a position past it snaps back.
    assert_eq!(editor.snap(u64::MAX, Snap::Bar), 11 * BAR);
    assert_eq!(editor.snap(LENGTH, Snap::Beat), LENGTH - BEAT);
}

#[test]
fn a_section_range_and_its_bars_come_from_the_markers_and_the_bar_lines() {
    let editor = editor();

    assert_eq!(editor.section_range("chorus"), Some(6 * BAR..10 * BAR));
    assert_eq!(editor.section_range("outro"), Some(10 * BAR..LENGTH));
    assert_eq!(editor.section_range("bridge"), None);
    assert_eq!(editor.bars_in(6 * BAR..10 * BAR), 4);
    assert_eq!(
        editor.bars_in(6 * BAR + 1..10 * BAR),
        3,
        "bars that start in it"
    );
    assert_eq!(editor.bars_in(6 * BAR..6 * BAR + 1), 1);
}

#[test]
fn adding_a_marker_snaps_it_and_splits_the_section_it_falls_in() {
    let mut editor = editor();

    let id = editor.add_marker(4 * BAR + 30_000, Snap::Bar).unwrap();

    assert_eq!(id, "verse-2");
    assert_eq!(
        ids(&editor),
        ["intro", "verse", "verse-2", "chorus", "outro"]
    );
    assert_eq!(
        editor.package().sections[2],
        marker("verse-2", SectionType::Verse, None, 4 * BAR),
        "the split section's type, with no number"
    );
    assert_eq!(editor.section_range("verse"), Some(2 * BAR..4 * BAR));
    assert_eq!(editor.section_range("verse-2"), Some(4 * BAR..6 * BAR));

    let id = editor.add_marker(8 * BAR + 30_000, Snap::Beat).unwrap();

    assert_eq!(id, "chorus-2");
    assert_eq!(editor.section_range(&id), Some(8 * BAR + BEAT..10 * BAR));
    song(&editor);
}

#[test]
fn a_split_follows_every_occurrence_so_every_arrangement_plays_the_same_audio() {
    let mut editor = editor();
    let before = [plan(&editor, "default"), plan(&editor, "live")];

    let id = editor.add_marker(7 * BAR, Snap::Bar).unwrap();

    assert_eq!(
        order(&editor, "default"),
        ["intro", "verse", "chorus", &id, "outro"]
    );
    assert_eq!(
        order(&editor, "live"),
        [
            "intro", "chorus", &id, "chorus", &id, "chorus", &id, "outro"
        ]
    );
    for (arrangement, before) in ["default", "live"].into_iter().zip(before) {
        let after = plan(&editor, arrangement);
        assert_eq!(audio(&after), audio(&before), "{arrangement}");
        assert_eq!(after.length, before.length, "{arrangement}");
        assert_eq!(after.clicks, before.clicks, "{arrangement}");
    }
}

#[test]
fn a_marker_is_not_added_where_one_already_starts() {
    let mut editor = editor();

    let error = refused(&mut editor, |editor| {
        editor.add_marker(2 * BAR + 100, Snap::Bar)
    });

    assert_eq!(
        error,
        SongEditError::MarkerExists {
            section: "verse".to_owned(),
            label: "Verse 1".to_owned(),
            at: 2 * BAR,
            position: BarBeat { bar: 3, beat: 1 },
        }
    );
    assert_eq!(
        error.to_string(),
        "\"Verse 1\" already starts at bar 3, beat 1; two markers cannot share a start"
    );
    let error = refused(&mut editor, |editor| editor.add_marker(10, Snap::Beat));
    assert!(
        matches!(error, SongEditError::MarkerExists { ref section, at: 0, .. } if section == "intro"),
        "{error:?}"
    );
}

#[test]
fn moving_a_marker_updates_every_arrangement_that_uses_that_section() {
    let mut editor = editor();

    let start = editor
        .move_marker("chorus", 7 * BAR + 10_000, Snap::Bar)
        .unwrap();

    assert_eq!(start, 7 * BAR);
    for (arrangement, choruses) in [("default", 1), ("live", 3)] {
        let positions = positions(&editor, arrangement);
        let chorus: Vec<_> = positions
            .iter()
            .filter(|position| position.0 == "chorus")
            .collect();
        assert_eq!(chorus.len(), choruses, "{arrangement}");
        for position in chorus {
            assert_eq!(
                (position.1, position.2),
                (7 * BAR, 3 * BAR),
                "{arrangement}"
            );
        }
    }
    let verse = positions(&editor, "default")[1].clone();
    assert_eq!(verse, ("verse".to_owned(), 2 * BAR, 5 * BAR));
}

#[test]
fn a_moved_marker_snaps_to_the_nearest_bar_or_beat() {
    let mut editor = editor();

    assert_eq!(
        editor.move_marker("chorus", 7 * BAR + 30_000, Snap::Beat),
        Ok(7 * BAR + BEAT)
    );
    assert_eq!(editor.package().sections[2].start, 7 * BAR + BEAT);
    assert_eq!(
        editor.move_marker("chorus", 5 * BAR + 50_000, Snap::Bar),
        Ok(6 * BAR)
    );
    assert_eq!(editor.section_range("chorus"), Some(6 * BAR..10 * BAR));
    // The last marker stays before the song end.
    assert_eq!(
        editor.move_marker("outro", u64::MAX, Snap::Bar),
        Ok(11 * BAR)
    );
    song(&editor);
}

#[test]
fn the_first_marker_cannot_be_moved_or_deleted() {
    let mut editor = editor();

    let moved = refused(&mut editor, |editor| {
        editor.move_marker("intro", BAR, Snap::Bar)
    });
    let deleted = refused(&mut editor, |editor| editor.delete_marker("intro"));

    assert_eq!(moved, SongEditError::FirstMarkerMove);
    assert_eq!(
        moved.to_string(),
        "the first marker starts the song; it cannot be moved"
    );
    assert_eq!(deleted, SongEditError::FirstMarkerDelete);
    assert_eq!(
        deleted.to_string(),
        "the first marker starts the song; it cannot be deleted"
    );
}

#[test]
fn a_marker_cannot_cross_or_land_on_its_neighbours() {
    let mut editor = editor();

    let onto_previous = refused(&mut editor, |editor| {
        editor.move_marker("verse", 10, Snap::Bar)
    });
    let onto_next = refused(&mut editor, |editor| {
        editor.move_marker("verse", 6 * BAR, Snap::Bar)
    });
    let past_next = refused(&mut editor, |editor| {
        editor.move_marker("verse", 9 * BAR, Snap::Beat)
    });

    assert_eq!(
        onto_previous,
        SongEditError::CrossesPrevious {
            section: "verse".to_owned(),
            label: "Verse 1".to_owned(),
            at: 0,
            previous: "intro".to_owned(),
            previous_label: "Intro".to_owned(),
        }
    );
    assert_eq!(
        onto_previous.to_string(),
        "\"Verse 1\" cannot move to or before \"Intro\"; a marker stays between its neighbours"
    );
    let next = |at| SongEditError::CrossesNext {
        section: "verse".to_owned(),
        label: "Verse 1".to_owned(),
        at,
        next: "chorus".to_owned(),
        next_label: "Chorus".to_owned(),
    };
    assert_eq!(onto_next, next(6 * BAR));
    assert_eq!(past_next, next(9 * BAR));
    assert_eq!(
        onto_next.to_string(),
        "\"Verse 1\" cannot move to or past \"Chorus\"; a marker stays between its neighbours"
    );
}

#[test]
fn a_marker_takes_a_type_from_the_list_and_an_optional_number() {
    let mut editor = editor();

    editor
        .set_marker_type("verse", SectionType::PreChorus, Some(2))
        .unwrap();

    assert_eq!(
        editor.package().sections[1],
        marker("verse", SectionType::PreChorus, Some(2), 2 * BAR)
    );
    editor
        .set_marker_type("verse", SectionType::Bridge, None)
        .unwrap();
    assert_eq!(
        editor.package().sections[1],
        marker("verse", SectionType::Bridge, None, 2 * BAR)
    );
    song(&editor);
}

#[test]
fn deleting_a_marker_removes_its_section_from_every_arrangement_and_loop_list() {
    let mut package = package();
    package.arrangements[1].infinite_loop = vec!["chorus".to_owned(), "outro".to_owned()];
    let mut editor = editor_of(package);

    editor.delete_marker("chorus").unwrap();

    assert_eq!(ids(&editor), ["intro", "verse", "outro"]);
    assert_eq!(order(&editor, "default"), ["intro", "verse", "outro"]);
    assert_eq!(order(&editor, "live"), ["intro", "outro"]);
    assert_eq!(arrangement(&editor, "live").infinite_loop, ["outro"]);
    assert_eq!(
        editor.section_range("verse"),
        Some(2 * BAR..10 * BAR),
        "the section before it runs on to the next marker"
    );
    song(&editor);
}

#[test]
fn a_delete_that_would_empty_an_arrangement_is_refused_and_names_it() {
    let mut package = package();
    package.arrangements.push(Arrangement {
        id: "choruses".to_owned(),
        name: "Chorus only".to_owned(),
        sections: vec!["chorus".to_owned(), "chorus".to_owned()],
        infinite_loop: Vec::new(),
    });
    let mut editor = editor_of(package);

    let error = refused(&mut editor, |editor| editor.delete_marker("chorus"));

    assert_eq!(
        error,
        SongEditError::EmptiesArrangement {
            section: "chorus".to_owned(),
            label: "Chorus".to_owned(),
            arrangement: "choruses".to_owned(),
            name: "Chorus only".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "deleting \"Chorus\" would leave arrangement \"Chorus only\" with no sections"
    );
}

#[test]
fn edits_naming_an_unknown_section_or_arrangement_are_refused() {
    let mut editor = editor();
    let section = SongEditError::UnknownSection {
        id: "bridge".to_owned(),
    };
    let arrangement = SongEditError::UnknownArrangement {
        id: "acoustic".to_owned(),
    };

    assert_eq!(
        refused(&mut editor, |e| e.move_marker("bridge", BAR, Snap::Bar)),
        section
    );
    assert_eq!(
        refused(&mut editor, |e| e.set_marker_type(
            "bridge",
            SectionType::Bridge,
            None
        )),
        section
    );
    assert_eq!(refused(&mut editor, |e| e.delete_marker("bridge")), section);
    assert_eq!(
        refused(&mut editor, |e| e.insert_section("default", 0, "bridge")),
        section
    );
    assert_eq!(
        refused(&mut editor, |e| e.append_section("default", "bridge")),
        section
    );
    assert_eq!(
        refused(&mut editor, |e| e
            .set_infinite_loop("default", "bridge", true)),
        section
    );
    assert_eq!(section.to_string(), "there is no section \"bridge\"");

    assert_eq!(
        refused(&mut editor, |e| e.rename_arrangement("acoustic", "A")),
        arrangement
    );
    assert_eq!(
        refused(&mut editor, |e| e.delete_arrangement("acoustic")),
        arrangement
    );
    assert_eq!(
        refused(&mut editor, |e| e.insert_section("acoustic", 0, "intro")),
        arrangement
    );
    assert_eq!(
        refused(&mut editor, |e| e.append_section("acoustic", "intro")),
        arrangement
    );
    assert_eq!(
        refused(&mut editor, |e| e.remove_section("acoustic", 0)),
        arrangement
    );
    assert_eq!(
        refused(&mut editor, |e| e.move_section("acoustic", 0, 1)),
        arrangement
    );
    assert_eq!(
        refused(&mut editor, |e| e
            .set_infinite_loop("acoustic", "intro", true)),
        arrangement
    );
    assert_eq!(
        arrangement.to_string(),
        "there is no arrangement \"acoustic\""
    );
    assert_eq!(editor.summary("acoustic"), None);
}

#[test]
fn a_new_arrangement_gets_a_unique_id_and_plays_every_section_once_in_order() {
    let mut editor = editor();

    let id = editor.new_arrangement("Live set").unwrap();
    // A different name with the same slug gets the next free id.
    let again = editor.new_arrangement("  Live-set ").unwrap();

    assert_eq!(id, "live-set");
    assert_eq!(again, "live-set-2");
    assert_eq!(
        arrangement(&editor, &again),
        &Arrangement {
            id: "live-set-2".to_owned(),
            name: "Live-set".to_owned(),
            sections: ["intro", "verse", "chorus", "outro"]
                .map(str::to_owned)
                .to_vec(),
            infinite_loop: Vec::new(),
        }
    );
    assert_eq!(editor.new_arrangement("Live!").unwrap(), "live-2");
    assert_eq!(editor.new_arrangement("¡¿!").unwrap(), "arrangement");
    song(&editor);
}

#[test]
fn an_arrangement_name_is_used_once_whatever_its_case() {
    let mut editor = editor();
    editor.new_arrangement("Live set").unwrap();

    let new = refused(&mut editor, |editor| editor.new_arrangement(" default "));
    let again = refused(&mut editor, |editor| editor.new_arrangement("LIVE SET"));
    let renamed = refused(&mut editor, |editor| {
        editor.rename_arrangement("live", "Live Set")
    });

    assert_eq!(
        new,
        SongEditError::NameTaken {
            name: "default".to_owned()
        }
    );
    assert_eq!(
        again,
        SongEditError::NameTaken {
            name: "Live set".to_owned()
        }
    );
    assert_eq!(
        renamed,
        SongEditError::NameTaken {
            name: "Live set".to_owned()
        }
    );
    assert_eq!(
        new.to_string(),
        "an arrangement named \"default\" already exists"
    );
    // Renaming an arrangement to its own name, in another case, is fine.
    editor.rename_arrangement("live", "LIVE").unwrap();
}

#[test]
fn an_arrangement_needs_a_name() {
    let mut editor = editor();

    let new = refused(&mut editor, |editor| editor.new_arrangement("   "));
    let renamed = refused(&mut editor, |editor| editor.rename_arrangement("live", ""));

    assert_eq!(new, SongEditError::EmptyName);
    assert_eq!(renamed, SongEditError::EmptyName);
    assert_eq!(new.to_string(), "an arrangement needs a name");
}

#[test]
fn an_arrangement_is_renamed_and_deleted_but_the_default_is_neither() {
    let mut editor = editor();

    editor.rename_arrangement("live", " Sunday ").unwrap();
    assert_eq!(arrangement(&editor, "live").name, "Sunday", "the id stays");
    editor.delete_arrangement("live").unwrap();
    let ids: Vec<&str> = editor
        .package()
        .arrangements
        .iter()
        .map(|arrangement| arrangement.id.as_str())
        .collect();
    assert_eq!(ids, ["default"]);

    let renamed = refused(&mut editor, |e| e.rename_arrangement("default", "Main"));
    let deleted = refused(&mut editor, |e| e.delete_arrangement("default"));

    assert_eq!(renamed, SongEditError::DefaultArrangementFixed);
    assert_eq!(deleted, SongEditError::DefaultArrangementFixed);
    assert_eq!(
        deleted.to_string(),
        "the Default arrangement cannot be renamed or deleted"
    );
}

#[test]
fn sections_are_inserted_appended_removed_and_moved_in_an_arrangement() {
    let mut editor = editor();

    editor.insert_section("default", 0, "outro").unwrap();
    assert_eq!(
        order(&editor, "default"),
        ["outro", "intro", "verse", "chorus", "outro"]
    );
    editor.append_section("default", "chorus").unwrap();
    assert_eq!(
        order(&editor, "default"),
        ["outro", "intro", "verse", "chorus", "outro", "chorus"]
    );
    assert_eq!(editor.remove_section("default", 1), Ok("intro".to_owned()));
    assert_eq!(
        order(&editor, "default"),
        ["outro", "verse", "chorus", "outro", "chorus"]
    );
    editor.move_section("default", 4, 0).unwrap();
    assert_eq!(
        order(&editor, "default"),
        ["chorus", "outro", "verse", "chorus", "outro"]
    );
    editor.move_section("default", 0, 2).unwrap();
    assert_eq!(
        order(&editor, "default"),
        ["outro", "verse", "chorus", "chorus", "outro"]
    );
    song(&editor);
}

#[test]
fn positions_outside_an_arrangement_are_refused() {
    let mut editor = editor();
    let out_of_range = |index| SongEditError::PositionOutOfRange {
        arrangement: "default".to_owned(),
        name: "default".to_owned(),
        index,
        len: 4,
    };

    let inserted = refused(&mut editor, |e| e.insert_section("default", 5, "intro"));

    assert_eq!(inserted, out_of_range(5));
    assert_eq!(
        inserted.to_string(),
        "position 5 is out of range for arrangement \"default\", which has 4 sections"
    );
    assert_eq!(
        refused(&mut editor, |e| e.remove_section("default", 4)),
        out_of_range(4)
    );
    assert_eq!(
        refused(&mut editor, |e| e.move_section("default", 4, 0)),
        out_of_range(4)
    );
    assert_eq!(
        refused(&mut editor, |e| e.move_section("default", 0, 4)),
        out_of_range(4)
    );
}

#[test]
fn the_only_section_of_an_arrangement_cannot_be_removed() {
    let mut package = package();
    package.arrangements.push(Arrangement {
        id: "solo".to_owned(),
        name: "Solo".to_owned(),
        sections: vec!["chorus".to_owned()],
        infinite_loop: Vec::new(),
    });
    let mut editor = editor_of(package);

    let error = refused(&mut editor, |editor| editor.remove_section("solo", 0));

    assert_eq!(
        error,
        SongEditError::LastPosition {
            arrangement: "solo".to_owned(),
            name: "Solo".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "arrangement \"Solo\" needs at least one section"
    );
}

#[test]
fn an_insert_that_makes_an_arrangement_too_long_is_refused() {
    let length = 13 * 3_600 * u64::from(RATE);
    let package = SongSpec::new(length, vec![marker("s", SectionType::Vamp, None, 0)]).package();
    let mut editor = SongEditor::new(package, length).unwrap();

    let error = refused(&mut editor, |editor| editor.append_section("default", "s"));

    assert_eq!(
        error,
        SongEditError::Package(vec![ValidationError::ArrangementTooLong {
            arrangement: "default".to_owned(),
            max: MAX_SONG_HOURS * 3_600 * u64::from(RATE),
        }])
    );
    assert!(
        error.to_string().contains("plays longer than 24 hours"),
        "{error}"
    );
}

#[test]
fn a_section_loops_only_where_it_plays_and_loses_the_flag_with_its_last_occurrence() {
    let mut editor = editor();

    editor.set_infinite_loop("live", "chorus", true).unwrap();
    editor.set_infinite_loop("live", "chorus", true).unwrap();
    assert_eq!(arrangement(&editor, "live").infinite_loop, ["chorus"]);

    let error = refused(&mut editor, |e| e.set_infinite_loop("live", "verse", true));
    assert_eq!(
        error,
        SongEditError::NotInArrangement {
            section: "verse".to_owned(),
            label: "Verse 1".to_owned(),
            arrangement: "live".to_owned(),
            name: "live".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "\"Verse 1\" is not in arrangement \"live\"; only its own sections can loop"
    );

    editor.move_section("live", 1, 3).unwrap();
    assert_eq!(arrangement(&editor, "live").infinite_loop, ["chorus"]);
    for left in [2, 1] {
        assert_eq!(editor.remove_section("live", 1), Ok("chorus".to_owned()));
        assert_eq!(
            arrangement(&editor, "live").infinite_loop,
            ["chorus"],
            "{left} choruses left"
        );
    }
    assert_eq!(editor.remove_section("live", 1), Ok("chorus".to_owned()));
    assert_eq!(order(&editor, "live"), ["intro", "outro"]);
    assert!(arrangement(&editor, "live").infinite_loop.is_empty());

    editor.set_infinite_loop("default", "outro", true).unwrap();
    assert_eq!(arrangement(&editor, "default").infinite_loop, ["outro"]);
    editor.set_infinite_loop("default", "outro", false).unwrap();
    assert!(arrangement(&editor, "default").infinite_loop.is_empty());
    song(&editor);
}

#[test]
fn the_summary_counts_sections_bars_and_whole_seconds() {
    let editor = editor();

    let default = editor.summary("default").unwrap();
    let live = editor.summary("live").unwrap();

    assert_eq!(
        default,
        ArrangementSummary {
            sections: 4,
            bars: 12,
            length_frames: LENGTH,
            sample_rate: RATE,
        }
    );
    assert_eq!(default.seconds(), 24);
    assert_eq!(default.to_string(), "4 sections · 12 bars · 0:24");
    assert_eq!(live.to_string(), "5 sections · 16 bars · 0:32");

    // A bar counts in the section it starts in, so a song ending inside a
    // bar counts that bar; seconds round down.
    let summary = |length: u64| {
        let package =
            SongSpec::new(length, vec![marker("s", SectionType::Vamp, None, 0)]).package();
        SongEditor::new(package, length)
            .unwrap()
            .summary("default")
            .unwrap()
            .to_string()
    };
    assert_eq!(summary(BAR + 1_000), "1 section · 2 bars · 0:02");
    assert_eq!(summary(BAR - 1), "1 section · 1 bar · 0:01");
    assert_eq!(
        summary(61 * 60 * 48_000 + 5 * 48_000),
        "1 section · 1833 bars · 61:05"
    );
}
