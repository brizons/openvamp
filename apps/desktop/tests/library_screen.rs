//! S4, F5.1–F5.5: the Library & setlists screen without a window, on a
//! temporary library folder. Every change is saved through the `library`
//! crate and read back from disk; the setlist the editor makes is the one
//! the `domain` crate validates and the setlist session plays.

use std::fs;
use std::path::Path;

use app::{SessionOptions, SetlistSession};
use domain::{
    Arrangement, ClickItem, SectionMarker, SectionType, Setlist, SetlistItem, SongItem,
    SongPackage, Stem, StemRole, TempoPoint, TimeSignature, TimeSignaturePoint, Transition,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::Library;
use openvamp_desktop::editor::{Edit, SetlistEditor};
use openvamp_desktop::library_page::{
    Choice, Dragged, Field, LibraryAction, LibraryPage, RowDetails, SongDetails,
};
use openvamp_desktop::line_edit::{LineEdit, LineKey, LineOutcome};
use openvamp_desktop::reorder::Half;
use openvamp_desktop::shelf::unique_name;

const RATE: u32 = 8_000;

/// A song of four one-second sections (intro, verse 1, chorus, outro) at
/// 8 kHz with two arrangements: Default plays all four, Short the verse
/// and the chorus.
fn write_song(dir: &Path, title: &str, key: Option<&str>, bpm: f64) {
    let section = |id: &str, section_type, number, second: u64| SectionMarker {
        id: id.to_owned(),
        section_type,
        number,
        start: second * u64::from(RATE),
    };
    let arrangement = |id: &str, name: &str, sections: &[&str]| Arrangement {
        id: id.to_owned(),
        name: name.to_owned(),
        sections: sections.iter().map(|&s| s.to_owned()).collect(),
        infinite_loop: Vec::new(),
    };
    let package = SongPackage {
        format_version: 1,
        title: title.to_owned(),
        artist: None,
        sample_rate: RATE,
        key: key.map(str::to_owned),
        tempo_map: vec![TempoPoint { at: 0, bpm }],
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
            section("intro", SectionType::Intro, None, 0),
            section("verse", SectionType::Verse, Some(1), 1),
            section("chorus", SectionType::Chorus, None, 2),
            section("outro", SectionType::Outro, None, 3),
        ],
        arrangements: vec![
            arrangement("default", "Default", &["intro", "verse", "chorus", "outro"]),
            arrangement("short", "Short", &["verse", "chorus"]),
        ],
    };
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(dir.join("keys.wav"), spec).unwrap();
    for _ in 0..4 * RATE {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();
}

/// A library with "Amazing" (A, 132 BPM) and "Bright" (no key, 90 BPM).
fn library() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_song(
        &dir.path().join("Songs/Amazing.song"),
        "Amazing",
        Some("A"),
        132.0,
    );
    write_song(&dir.path().join("Songs/Bright.song"), "Bright", None, 90.0);
    dir
}

fn save(root: &Path, name: &str, items: Vec<SetlistItem>) -> String {
    let mut setlist = Setlist::new(name);
    setlist.items = items;
    Library::new(root).save_setlist(&setlist).unwrap().file
}

/// The setlist in the editor as the library reads it back from disk.
fn on_disk(page: &LibraryPage) -> Setlist {
    let file = page.selected_file().expect("a setlist is in the editor");
    let saved = Library::new(page.root()).load_setlist(file).unwrap();
    assert_eq!(&saved, page.editor().unwrap().setlist(), "saved as edited");
    saved
}

fn rename_to(page: &mut LibraryPage, name: &str) {
    assert_eq!(page.typing(), Some(Field::Rename));
    let old = page.view(None).editor.unwrap().renaming.unwrap();
    for _ in old.chars() {
        page.type_key(LineKey::Backspace);
    }
    page.type_key(LineKey::Text(name));
    page.type_key(LineKey::Enter);
    assert_eq!(page.typing(), None);
}

fn setlist_names(page: &LibraryPage) -> Vec<String> {
    page.view(None)
        .setlists
        .into_iter()
        .map(|s| s.name)
        .collect()
}

fn files(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = fs::read_dir(root.join("Setlists"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    files
}

#[test]
fn setlists_are_created_renamed_and_duplicated_in_the_library_folder() {
    let dir = library();
    let mut page = LibraryPage::open(dir.path(), None).unwrap();
    assert!(page.view(None).setlists.is_empty());
    assert_eq!(page.view(None).editor, None);

    page.act(LibraryAction::NewSetlist);
    assert_eq!(setlist_names(&page), ["New setlist"]);
    assert_eq!(
        page.view(None).editor.unwrap().renaming.as_deref(),
        Some("New setlist")
    );
    rename_to(&mut page, "Sunday");

    assert_eq!(setlist_names(&page), ["Sunday"]);
    assert_eq!(page.selected_file(), Some("Sunday.setlist.json"));
    assert_eq!(files(dir.path()), ["Sunday.setlist.json"]);
    assert_eq!(on_disk(&page).name, "Sunday");

    page.act(LibraryAction::AddClick);
    page.act(LibraryAction::DuplicateSetlist);
    assert_eq!(page.typing(), Some(Field::Rename));
    page.type_key(LineKey::Escape);
    assert_eq!(page.selected_file(), Some("Sunday copy.setlist.json"));
    assert_eq!(on_disk(&page).items.len(), 1, "the copy holds the items");

    page.act(LibraryAction::NewSetlist);
    page.stop_typing();
    assert_eq!(
        setlist_names(&page),
        ["New setlist", "Sunday", "Sunday copy"]
    );
    let rows = page.view(None).setlists;
    assert_eq!(rows[1].meta, "1 item");
    assert_eq!(rows[0].meta, "0 items");
    assert!(rows[0].selected);

    // A name another setlist has is refused and says so; nothing moves.
    page.act(LibraryAction::RenameSetlist);
    rename_to(&mut page, "Sunday");
    assert!(
        page.notice()
            .unwrap()
            .contains("another setlist already uses"),
        "{:?}",
        page.notice()
    );
    assert_eq!(
        setlist_names(&page),
        ["New setlist", "Sunday", "Sunday copy"]
    );

    page.act(LibraryAction::SelectSetlist(2));
    assert_eq!(page.selected_file(), Some("Sunday copy.setlist.json"));
}

#[test]
fn items_are_added_removed_and_reordered_and_each_change_is_saved() {
    let dir = library();
    let file = save(dir.path(), "Sunday", Vec::new());
    let mut page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    let songs = page.view(None).songs;
    assert_eq!(
        songs.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        ["Amazing", "Bright"]
    );

    page.act(LibraryAction::AddSong(1));
    page.act(LibraryAction::AddSong(0));
    page.act(LibraryAction::AddClick);
    let saved = on_disk(&page);
    assert_eq!(
        saved.items,
        [
            SetlistItem::Song(SongItem::new("Songs/Bright.song")),
            SetlistItem::Song(SongItem::new("Songs/Amazing.song")),
            SetlistItem::Click(ClickItem::new(
                120.0,
                TimeSignature {
                    numerator: 4,
                    denominator: 4
                }
            )),
        ]
    );
    // The item added shows its details.
    let editor = page.view(None).editor.unwrap();
    assert!(editor.rows[2].selected);

    page.act(LibraryAction::MoveUp(2));
    page.act(LibraryAction::MoveDown(0));
    let titles = |page: &LibraryPage| {
        page.view(None)
            .editor
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(&page), ["Click", "Bright", "Amazing"]);
    on_disk(&page);

    page.act(LibraryAction::Remove(1));
    assert_eq!(titles(&page), ["Click", "Amazing"]);
    assert_eq!(on_disk(&page).items.len(), 2);

    // The ends of the list stay put.
    page.act(LibraryAction::MoveUp(0));
    page.act(LibraryAction::MoveDown(1));
    assert_eq!(titles(&page), ["Click", "Amazing"]);
}

#[test]
fn a_song_gets_its_arrangement_start_end_fade_out_and_transition() {
    let dir = library();
    let file = save(
        dir.path(),
        "Sunday",
        vec![SetlistItem::Song(SongItem::new("Songs/Amazing.song"))],
    );
    let mut page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    page.act(LibraryAction::SelectRow(0));
    let details = |page: &LibraryPage| match page.view(None).editor.unwrap().rows[0].details.clone()
    {
        Some(RowDetails::Song(details)) => details,
        other => panic!("song details expected: {other:?}"),
    };
    assert_eq!(
        details(&page),
        SongDetails {
            arrangements: vec![
                Choice {
                    label: "Default".to_owned(),
                    selected: true
                },
                Choice {
                    label: "Short".to_owned(),
                    selected: false
                },
            ],
            start: "1 · Intro".to_owned(),
            end: "4 · Outro".to_owned(),
            start_earlier: false,
            start_later: true,
            end_earlier: true,
            end_later: false,
            fade_out: false,
        }
    );

    page.act(LibraryAction::StartLater(0));
    page.act(LibraryAction::EndEarlier(0));
    page.act(LibraryAction::ToggleFadeOut(0));
    page.act(LibraryAction::Transition(0, Transition::Crossfade));
    let item = SongItem {
        start_position: Some(1),
        end_position: Some(2),
        fade_out: true,
        transition: Transition::Crossfade,
        ..SongItem::new("Songs/Amazing.song")
    };
    assert_eq!(on_disk(&page).items, [SetlistItem::Song(item.clone())]);
    let shown = details(&page);
    assert_eq!(
        (shown.start.as_str(), shown.end.as_str()),
        ("2 · Verse 1", "3 · Chorus")
    );
    let row = &page.view(None).editor.unwrap().rows[0];
    assert_eq!(
        row.meta,
        // The arrangement has its own column, so the meta line leaves it out.
        "A · 132 BPM · Verse 1 – Chorus · fade-out · 1 stem"
    );
    assert_eq!(row.length, "0:02");

    // The start never passes the end.
    page.act(LibraryAction::StartLater(0));
    page.act(LibraryAction::StartLater(0));
    assert_eq!(details(&page).start, "3 · Chorus");
    assert!(!details(&page).start_later && !details(&page).end_earlier);

    // Another arrangement plays in full again.
    page.act(LibraryAction::Arrangement(0, 1));
    assert_eq!(
        on_disk(&page).items,
        [SetlistItem::Song(SongItem {
            arrangement: "short".to_owned(),
            start_position: None,
            end_position: None,
            ..item
        })]
    );
    assert_eq!(details(&page).end, "2 · Chorus");
}

#[test]
fn a_click_tile_gets_its_tempo_meter_bars_and_transition() {
    let dir = library();
    let file = save(dir.path(), "Sunday", Vec::new());
    let mut page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    page.act(LibraryAction::AddClick);

    page.act(LibraryAction::BpmDown(0));
    page.act(LibraryAction::BpmDown(0));
    page.act(LibraryAction::Meter(0, 4));
    page.act(LibraryAction::BarsUp(0));
    page.act(LibraryAction::BarsUp(0));
    page.act(LibraryAction::Transition(0, Transition::AutoLink));

    assert_eq!(
        on_disk(&page).items,
        [SetlistItem::Click(ClickItem {
            bpm: 118.0,
            time_signature: TimeSignature {
                numerator: 6,
                denominator: 8
            },
            bars: Some(2),
            transition: Transition::AutoLink,
        })]
    );
    let row = page.view(None).editor.unwrap().rows[0].clone();
    assert_eq!(row.meta, "118 BPM · 6/8 · 2 bars");
    // Two bars of six eighth notes at 118 eighths a minute.
    assert_eq!(row.length, "0:06");
    let Some(RowDetails::Click(details)) = row.details else {
        panic!("click details expected");
    };
    assert_eq!(details.bpm, "118 BPM");
    assert_eq!(details.bars, "2 bars");
    assert!(details.meters[4].selected && details.meters[4].label == "6/8");

    page.act(LibraryAction::BarsDown(0));
    page.act(LibraryAction::BarsDown(0));
    let row = page.view(None).editor.unwrap().rows[0].clone();
    assert_eq!(row.meta, "118 BPM · 6/8 · until you move on");
    assert_eq!(row.length, "—");
}

#[test]
fn the_rules_a_setlist_breaks_show_as_the_domain_reports_them() {
    let dir = library();
    let file = save(
        dir.path(),
        "Sunday",
        vec![
            SetlistItem::Song(SongItem::new("Songs/Gone.song")),
            SetlistItem::Song(SongItem {
                start_position: Some(3),
                end_position: Some(1),
                ..SongItem::new("Songs/Bright.song")
            }),
        ],
    );
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();

    let editor = page.view(None).editor.unwrap();
    assert_eq!(
        editor.rows[0].problems,
        ["item 1 names song \"Songs/Gone.song\", which is not in the library"]
    );
    assert_eq!(
        editor.rows[1].problems,
        ["item 2 has start position 3 after its end position 1"]
    );
    assert_eq!(editor.rows[0].title, "Gone");
    assert_eq!(editor.rows[0].length, "—");
    assert!(editor.problems.is_empty());
}

#[test]
fn the_setlist_made_is_the_one_the_setlist_session_plays() {
    let dir = library();
    let file = save(dir.path(), "Sunday", Vec::new());
    let mut page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    page.act(LibraryAction::AddSong(0));
    page.act(LibraryAction::Transition(0, Transition::AutoLink));
    page.act(LibraryAction::AddClick);
    page.act(LibraryAction::BarsUp(1));
    page.act(LibraryAction::AddSong(1));
    page.act(LibraryAction::Arrangement(2, 1));

    let prepared =
        SetlistSession::prepare(&Library::new(dir.path()), &file, SessionOptions::default())
            .unwrap();

    let info = prepared.info();
    assert_eq!(info.name, "Sunday");
    assert!(info.unavailable().is_empty(), "{:?}", info.items);
    assert_eq!(info.items.len(), 3);
    assert_eq!(info.items[0].item.transition(), Transition::AutoLink);
}

#[test]
fn a_setlist_in_a_file_named_by_hand_is_saved_in_one_file() {
    let dir = library();
    let mut setlist = Setlist::new("Sunday");
    setlist.items = vec![SetlistItem::Song(SongItem::new("Songs/Amazing.song"))];
    fs::create_dir_all(dir.path().join("Setlists")).unwrap();
    fs::write(
        dir.path().join("Setlists/by hand.setlist.json"),
        setlist.to_json().unwrap(),
    )
    .unwrap();
    let mut editor = SetlistEditor::open(Library::new(dir.path()), "by hand.setlist.json").unwrap();

    editor.apply(Edit::FadeOut(0, true)).unwrap();

    assert_eq!(files(dir.path()), ["Sunday.setlist.json"]);
    assert_eq!(editor.file(), "Sunday.setlist.json");
    let saved = Library::new(dir.path())
        .load_setlist("Sunday.setlist.json")
        .unwrap();
    assert_eq!(&saved, editor.setlist());
}

#[test]
fn a_refused_edit_changes_nothing() {
    let dir = library();
    let file = save(
        dir.path(),
        "Sunday",
        vec![SetlistItem::Song(SongItem::new("Songs/Amazing.song"))],
    );
    let mut editor = SetlistEditor::open(Library::new(dir.path()), &file).unwrap();
    let before = editor.setlist().clone();

    let error = editor.apply(Edit::Remove(4)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "item index 4 is out of range for a setlist of 1 items"
    );
    let error = editor
        .apply(Edit::Click(
            0,
            ClickItem::new(
                90.0,
                TimeSignature {
                    numerator: 3,
                    denominator: 4,
                },
            ),
        ))
        .unwrap_err();
    assert_eq!(error.to_string(), "item 1 is a song, not a click-only tile");

    assert_eq!(editor.setlist(), &before);
    assert_eq!(
        Library::new(dir.path()).load_setlist(&file).unwrap(),
        before
    );
}

#[test]
fn the_song_list_shows_the_library_and_filters_by_title_key_or_tempo() {
    let dir = library();
    let mut page = LibraryPage::open(dir.path(), None).unwrap();

    let view = page.view(None);
    assert_eq!(view.song_summary, "2 songs");
    let amazing = &view.songs[0];
    assert_eq!(
        (
            amazing.title.as_str(),
            amazing.key.as_str(),
            amazing.bpm.as_str(),
            amazing.length.as_str(),
            amazing.stems.as_str(),
            amazing.path.as_str()
        ),
        (
            "Amazing",
            "A",
            "132",
            "0:04",
            "1 stem",
            "Songs/Amazing.song"
        )
    );
    assert_eq!(view.songs[1].key, "—");

    page.act(LibraryAction::Search);
    assert_eq!(page.typing(), Some(Field::Search));
    page.type_key(LineKey::Text("9"));
    let view = page.view(None);
    assert_eq!(view.search, "9");
    assert_eq!(view.song_summary, "1 of 2 songs");
    assert_eq!(view.songs[0].title, "Bright");
    // Adding from a filtered list adds the song shown.
    page.act(LibraryAction::NewSetlist);
    page.stop_typing();
    page.act(LibraryAction::AddSong(0));
    assert_eq!(
        on_disk(&page).items,
        [SetlistItem::Song(SongItem::new("Songs/Bright.song"))]
    );

    page.act(LibraryAction::Search);
    page.type_key(LineKey::Backspace);
    page.type_key(LineKey::Text("bri"));
    assert_eq!(page.view(None).songs.len(), 1);
    page.type_key(LineKey::Escape);
    assert_eq!(page.view(None).search, "");
    assert_eq!(page.view(None).songs.len(), 2);
}

#[test]
fn the_editor_says_when_the_setlist_is_the_one_on_perform() {
    let dir = library();
    let file = save(dir.path(), "Sunday", Vec::new());
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();

    let view = page.view(Some(&file));
    assert!(view.editor.unwrap().playing);
    assert!(view.setlists[0].playing);
    assert!(!page.view(None).editor.unwrap().playing);
    assert_eq!(page.view(None).editor.unwrap().summary, "No items yet");
}

#[test]
fn a_line_edit_types_deletes_commits_and_cancels() {
    let mut field = LineEdit::new("Sun");
    assert_eq!(field.key(LineKey::Text("day")), LineOutcome::Editing);
    assert_eq!(field.text(), "Sunday");
    assert_eq!(field.key(LineKey::Backspace), LineOutcome::Editing);
    field.paste("s\nignored");
    assert_eq!(field.text(), "Sundas");
    field.paste("\u{7}é");
    assert_eq!(
        field.key(LineKey::Enter),
        LineOutcome::Commit("Sundasé".to_owned())
    );
    assert_eq!(field.key(LineKey::Escape), LineOutcome::Cancel);

    assert_eq!(
        LineKey::from_key("backspace", None),
        Some(LineKey::Backspace)
    );
    assert_eq!(LineKey::from_key("enter", None), Some(LineKey::Enter));
    assert_eq!(LineKey::from_key("escape", None), Some(LineKey::Escape));
    assert_eq!(LineKey::from_key("a", Some("A")), Some(LineKey::Text("A")));
    assert_eq!(
        LineKey::from_key("space", Some(" ")),
        Some(LineKey::Text(" "))
    );
    assert_eq!(LineKey::from_key("left", None), None);
}

#[test]
fn a_name_is_made_unique_with_a_number_ignoring_case() {
    assert_eq!(unique_name("New setlist", &[]), "New setlist");
    assert_eq!(
        unique_name("New setlist", &["new SETLIST", "New setlist 2"]),
        "New setlist 3"
    );
}

#[test]
fn while_a_field_has_the_focus_keys_type_into_it_except_shortcuts() {
    use openvamp_desktop::keys::{KeyAction, KeyPress, Mods, SECONDARY_IS_PLATFORM, Typed, typed};

    let plain = |key| KeyPress {
        key,
        mods: Mods::default(),
    };
    let secondary = Mods {
        platform: SECONDARY_IS_PLATFORM,
        control: !SECONDARY_IS_PLATFORM,
        ..Mods::default()
    };
    // Space, x and digits type rather than play, panic or queue.
    assert_eq!(
        typed(&plain("space"), Some(" ")),
        Typed::Line(LineKey::Text(" "))
    );
    assert_eq!(
        typed(&plain("x"), Some("x")),
        Typed::Line(LineKey::Text("x"))
    );
    assert_eq!(
        typed(&plain("1"), Some("1")),
        Typed::Line(LineKey::Text("1"))
    );
    assert_eq!(
        typed(&plain("backspace"), None),
        Typed::Line(LineKey::Backspace)
    );
    assert_eq!(typed(&plain("left"), None), Typed::Ignored);
    let with_secondary = |key| KeyPress {
        key,
        mods: secondary,
    };
    assert_eq!(typed(&with_secondary("v"), Some("v")), Typed::Paste);
    assert_eq!(
        typed(&with_secondary("q"), Some("q")),
        Typed::Key(KeyAction::Quit)
    );
    assert_eq!(
        typed(&with_secondary("o"), Some("o")),
        Typed::Key(KeyAction::OpenSong)
    );
    assert_eq!(typed(&with_secondary("a"), Some("a")), Typed::Ignored);
}

#[test]
fn setlists_move_up_and_down_and_the_order_lasts() {
    let dir = library();
    for name in ["A", "B", "C"] {
        save(dir.path(), name, Vec::new());
    }
    let mut page = LibraryPage::open(dir.path(), None).unwrap();
    assert_eq!(page.selected_file(), Some("A.setlist.json"));

    page.act(LibraryAction::MoveSetlistDown(0));
    assert_eq!(setlist_names(&page), ["B", "A", "C"]);
    page.act(LibraryAction::MoveSetlistUp(2));
    assert_eq!(setlist_names(&page), ["B", "C", "A"]);
    // The ends of the list stay put.
    page.act(LibraryAction::MoveSetlistUp(0));
    page.act(LibraryAction::MoveSetlistDown(2));
    assert_eq!(setlist_names(&page), ["B", "C", "A"]);
    assert_eq!(page.notice(), None);
    // The setlist in the editor stays there, marked where it moved.
    assert_eq!(page.selected_file(), Some("A.setlist.json"));
    assert!(page.view(None).setlists[2].selected);

    let reopened = LibraryPage::open(dir.path(), None).unwrap();
    assert_eq!(setlist_names(&reopened), ["B", "C", "A"]);

    // With an order stored, a new setlist goes at the end and a copy
    // right after its original.
    page.act(LibraryAction::NewSetlist);
    page.stop_typing();
    page.act(LibraryAction::SelectSetlist(0));
    page.act(LibraryAction::DuplicateSetlist);
    page.stop_typing();
    assert_eq!(
        setlist_names(&page),
        ["B", "B copy", "C", "A", "New setlist"]
    );
}

#[test]
fn an_order_that_cannot_be_used_is_reported_and_setlists_are_listed_by_name() {
    let dir = library();
    for name in ["C", "A", "B"] {
        save(dir.path(), name, Vec::new());
    }
    fs::write(dir.path().join("Setlists/order.json"), "{ broken").unwrap();

    let page = LibraryPage::open(dir.path(), None).unwrap();

    let view = page.view(None);
    assert_eq!(setlist_names(&page), ["A", "B", "C"]);
    let problem = view.order_problem.expect("the problem shows");
    assert!(
        problem.contains("is not a valid setlist order") && problem.contains("listed by name"),
        "{problem}"
    );
}

/// Fills every name the library's temporary file for `Setlists/order.json`
/// can take (`.order.json.<pid>-<n>.tmp`, n from 0 to 99) with a folder, so
/// storing the order fails while setlists are still written.
fn block_order_writes(root: &Path) {
    for attempt in 0..100 {
        let name = format!(".order.json.{}-{attempt}.tmp", std::process::id());
        fs::create_dir(root.join("Setlists").join(name)).unwrap();
    }
}

#[test]
fn a_setlist_saved_whose_place_cannot_be_stored_is_listed_with_a_warning() {
    let dir = library();
    for name in ["A", "B"] {
        save(dir.path(), name, Vec::new());
    }
    let mut page = LibraryPage::open(dir.path(), None).unwrap();
    page.act(LibraryAction::MoveSetlistDown(0));
    assert_eq!(setlist_names(&page), ["B", "A"]);
    block_order_writes(dir.path());

    page.act(LibraryAction::NewSetlist);
    assert_eq!(page.notice(), None, "not a failed edit");
    let warning = page.view(None).warning.expect("a warning shows");
    assert!(
        warning.starts_with("setlist \"New setlist\" was saved as New setlist.setlist.json"),
        "{warning}"
    );
    assert_eq!(page.selected_file(), Some("New setlist.setlist.json"));
    assert_eq!(page.typing(), Some(Field::Rename));
    // Not in the stored order, it has no place to keep when renamed.
    rename_to(&mut page, "Sunday");
    assert_eq!(page.notice(), None);
    assert_eq!(page.selected_file(), Some("Sunday.setlist.json"));
    assert_eq!(setlist_names(&page), ["B", "A", "Sunday"]);

    // In the stored order, renaming and duplicating warn the same way.
    page.act(LibraryAction::SelectSetlist(1));
    page.act(LibraryAction::RenameSetlist);
    rename_to(&mut page, "Zed");
    assert_eq!(page.notice(), None);
    assert_eq!(page.selected_file(), Some("Zed.setlist.json"));
    let warning = page.view(None).warning.unwrap();
    assert!(
        warning.starts_with("setlist \"Zed\" was saved"),
        "{warning}"
    );

    page.act(LibraryAction::SelectSetlist(0));
    page.act(LibraryAction::DuplicateSetlist);
    page.stop_typing();
    assert_eq!(page.notice(), None);
    assert_eq!(page.selected_file(), Some("B copy.setlist.json"));
    let warning = page.view(None).warning.unwrap();
    assert!(
        warning.starts_with("setlist \"B copy\" was saved"),
        "{warning}"
    );
    assert_eq!(setlist_names(&page), ["B", "B copy", "Sunday", "Zed"]);

    page.act(LibraryAction::DismissNotice);
    assert_eq!(page.view(None).warning, None);
}

#[test]
fn adding_a_song_with_no_setlist_in_the_editor_changes_nothing() {
    let dir = library();
    let mut page = LibraryPage::open(dir.path(), None).unwrap();
    assert!(page.editor().is_none());
    page.act(LibraryAction::AddSong(0));
    assert!(page.editor().is_none());
    assert!(!dir.path().join("Setlists").exists());
    page.act(LibraryAction::AddClick);
    assert!(page.editor().is_none());
    assert!(!dir.path().join("Setlists").exists());
}

#[test]
fn values_a_file_can_hold_never_crash_the_view_and_show_the_domain_problem() {
    let dir = library();
    let file = save(
        dir.path(),
        "Sunday",
        vec![
            SetlistItem::Click(ClickItem {
                bars: Some(4),
                ..ClickItem::new(
                    0.0,
                    TimeSignature {
                        numerator: 4,
                        denominator: 4,
                    },
                )
            }),
            SetlistItem::Song(SongItem {
                start_position: Some(usize::MAX),
                end_position: Some(usize::MAX),
                ..SongItem::new("Songs/Amazing.song")
            }),
            SetlistItem::Click(ClickItem {
                bars: Some(u32::MAX),
                ..ClickItem::new(
                    1.0,
                    TimeSignature {
                        numerator: u32::MAX,
                        denominator: 4,
                    },
                )
            }),
        ],
    );

    let mut page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    page.act(LibraryAction::SelectRow(1));

    let editor = page.view(None).editor.unwrap();
    assert_eq!(editor.rows[0].length, "—");
    assert_eq!(
        editor.rows[0].problems,
        ["item 1 (click-only) has bpm 0; bpm must be from 1 to 999"]
    );
    assert_eq!(editor.rows[1].length, "—");
    assert!(!editor.rows[1].problems.is_empty());
    assert!(
        editor.summary.starts_with("3 items · at least "),
        "{}",
        editor.summary
    );
    page.act(LibraryAction::SelectRow(2));
    page.view(None);
}

#[test]
fn renaming_reports_the_move_so_perform_can_follow_it() {
    let dir = library();
    let file = save(dir.path(), "Sunday", Vec::new());
    let mut page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    assert_eq!(page.take_moves(), Vec::<(String, String)>::new());

    page.act(LibraryAction::RenameSetlist);
    rename_to(&mut page, "Zed");

    assert_eq!(
        page.take_moves(),
        [(
            "Sunday.setlist.json".to_owned(),
            "Zed.setlist.json".to_owned()
        )]
    );
    assert_eq!(page.take_moves(), Vec::<(String, String)>::new());
    assert!(page.view(Some("Zed.setlist.json")).setlists[0].playing);

    // A file named by hand moves on its first change, and says so.
    let mut setlist = Setlist::new("Evening");
    setlist.items = vec![SetlistItem::Click(ClickItem::new(
        100.0,
        TimeSignature {
            numerator: 4,
            denominator: 4,
        },
    ))];
    fs::write(
        dir.path().join("Setlists/by hand.setlist.json"),
        setlist.to_json().unwrap(),
    )
    .unwrap();
    let mut page = LibraryPage::open(dir.path(), Some("by hand.setlist.json")).unwrap();
    page.act(LibraryAction::Transition(0, Transition::Stay));
    assert_eq!(
        page.take_moves(),
        [(
            "by hand.setlist.json".to_owned(),
            "Evening.setlist.json".to_owned()
        )]
    );
}

#[test]
fn a_followed_library_follows_the_playing_setlist_and_a_picked_one_stays() {
    use openvamp_desktop::library_page::{Opened, library_to_open};

    let library = Path::new("/music/Library");
    let other = Path::new("/music/Other");
    let playing = Some((library, "Sunday.setlist.json"));
    let opens = Some((library.to_owned(), "Sunday.setlist.json".to_owned()));

    // Nothing open yet: the playing setlist's library.
    assert_eq!(library_to_open(None, playing), opens);
    // Opened by following a setlist: it follows the one playing now.
    assert_eq!(
        library_to_open(Some((other, Opened::Followed)), playing),
        opens
    );
    assert_eq!(
        library_to_open(Some((library, Opened::Followed)), playing),
        None
    );
    // Picked with Open library…: it stays.
    assert_eq!(
        library_to_open(Some((other, Opened::Picked)), playing),
        None
    );
    // Nothing playing: nothing changes.
    assert_eq!(library_to_open(None, None), None);
    assert_eq!(library_to_open(Some((other, Opened::Followed)), None), None);
}

#[test]
fn a_library_opens_at_its_canonical_folder() {
    let dir = library();
    let page = LibraryPage::open(&dir.path().join("Songs/.."), None).unwrap();
    assert_eq!(page.root(), fs::canonicalize(dir.path()).unwrap());
}

/// The test trash: `LIBRARY/Trash`, so no test fills the real trash.
fn test_trash(path: &Path) -> std::io::Result<()> {
    let library = path
        .parent()
        .and_then(Path::parent)
        .expect("a setlist file is in LIBRARY/Setlists");
    let trash = library.join("Trash");
    fs::create_dir_all(&trash)?;
    fs::rename(path, trash.join(path.file_name().expect("a file name")))
}

/// A library with setlists A, B and C, open on `selected`.
fn removable(selected: &str) -> (tempfile::TempDir, LibraryPage) {
    let dir = library();
    for name in ["A", "B", "C"] {
        save(dir.path(), name, Vec::new());
    }
    let page = LibraryPage::open_with_trash(
        dir.path(),
        Some(&format!("{selected}.setlist.json")),
        test_trash,
    )
    .unwrap();
    (dir, page)
}

/// The armed remove control's label: short, since the setlist's name is in
/// the editor's header (or the file in the line above an unreadable one)
/// and a long label squeezes the name out of the header.
const ARMED: &str = "Click again to remove";

fn remove_label(page: &LibraryPage) -> String {
    page.view(None)
        .editor
        .expect("a setlist is selected")
        .remove
}

#[test]
fn removing_a_setlist_takes_a_second_click_and_moves_it_to_the_trash() {
    let (dir, mut page) = removable("B");
    assert_eq!(remove_label(&page), "Remove");

    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(remove_label(&page), ARMED);
    assert!(page.view(None).editor.unwrap().remove_armed);
    assert_eq!(
        setlist_names(&page),
        ["A", "B", "C"],
        "one click removes nothing"
    );
    assert!(dir.path().join("Setlists/B.setlist.json").exists());

    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(page.notice(), None);
    assert_eq!(setlist_names(&page), ["A", "C"]);
    assert!(!dir.path().join("Setlists/B.setlist.json").exists());
    assert!(dir.path().join("Trash/B.setlist.json").exists());
    // The next setlist takes its place in the editor, not armed.
    assert_eq!(page.selected_file(), Some("C.setlist.json"));
    assert_eq!(remove_label(&page), "Remove");
}

#[test]
fn any_other_action_disarms_the_removal() {
    let (dir, mut page) = removable("B");
    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::AddClick);
    assert_eq!(remove_label(&page), "Remove");
    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(remove_label(&page), ARMED);
    assert!(dir.path().join("Setlists/B.setlist.json").exists());

    // Changing the selection disarms too: the second click arms A.
    page.act(LibraryAction::SelectSetlist(0));
    assert_eq!(remove_label(&page), "Remove");
    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(remove_label(&page), ARMED);
    assert_eq!(page.selected_file(), Some("A.setlist.json"), "armed for A");
    assert_eq!(setlist_names(&page), ["A", "B", "C"]);

    // So does typing into a field.
    page.act(LibraryAction::Search);
    page.type_key(LineKey::Text("x"));
    assert_eq!(remove_label(&page), "Remove");
}

#[test]
fn the_setlist_on_perform_cannot_be_removed() {
    let (dir, mut page) = removable("B");
    page.set_playing(Some("B.setlist.json"));

    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::RemoveSetlist);

    assert_eq!(
        page.notice(),
        Some(
            "\"B\" is open on Perform, so it cannot be removed: open another setlist or a song there first."
        )
    );
    assert_eq!(remove_label(&page), "Remove");
    assert_eq!(setlist_names(&page), ["A", "B", "C"]);
    assert!(dir.path().join("Setlists/B.setlist.json").exists());

    // Once another setlist plays there, it can be removed.
    page.set_playing(Some("A.setlist.json"));
    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(setlist_names(&page), ["A", "C"]);
}

#[test]
fn after_removing_the_last_setlist_the_previous_one_or_none_is_selected() {
    let (_dir, mut page) = removable("C");
    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(page.selected_file(), Some("B.setlist.json"));

    for _ in 0..2 {
        page.act(LibraryAction::RemoveSetlist);
        page.act(LibraryAction::RemoveSetlist);
    }
    assert_eq!(setlist_names(&page), Vec::<String>::new());
    assert_eq!(page.selected_file(), None);
    assert_eq!(page.view(None).editor, None);
}

#[test]
fn removing_keeps_the_stored_order_of_the_rest() {
    let (dir, mut page) = removable("A");
    page.act(LibraryAction::MoveSetlistDown(0));
    page.act(LibraryAction::MoveSetlistDown(1));
    assert_eq!(setlist_names(&page), ["B", "C", "A"]);
    page.act(LibraryAction::SelectSetlist(1));

    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::RemoveSetlist);

    assert_eq!(setlist_names(&page), ["B", "A"]);
    assert_eq!(page.selected_file(), Some("A.setlist.json"));
    let reopened = LibraryPage::open_with_trash(dir.path(), None, test_trash).unwrap();
    assert_eq!(setlist_names(&reopened), ["B", "A"]);
}

#[test]
fn the_setlist_on_perform_is_refused_whatever_case_it_was_opened_in() {
    use app::testing::FakeOutput;
    use app::{AudioOutput, SessionOptions};
    use openvamp_desktop::show::{Source, replace};

    let dir = library();
    for name in ["A", "B", "C"] {
        save(
            dir.path(),
            name,
            vec![SetlistItem::Song(SongItem::new("Songs/Amazing.song"))],
        );
    }
    if !dir.path().join("Setlists/b.setlist.json").exists() {
        return; // a case-sensitive file system has no such variant
    }
    let source = Source::for_path(&dir.path().join("Setlists/b.setlist.json")).unwrap();
    let fake = FakeOutput::new();
    let mut current = None;
    replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        || Box::new(fake.clone()) as Box<dyn AudioOutput>,
        |_| {},
    )
    .unwrap();
    let playing = current.as_ref().unwrap().setlist().unwrap().file.clone();
    let mut page =
        LibraryPage::open_with_trash(dir.path(), Some("B.setlist.json"), test_trash).unwrap();
    page.set_playing(Some(&playing));
    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::RemoveSetlist);
    assert!(dir.path().join("Setlists/B.setlist.json").exists());
    assert!(page.notice().is_some_and(|n| n.contains("open on Perform")));

    // The list marks it as the one on Perform, and so does the editor.
    assert_eq!(playing, "B.setlist.json", "the name as the folder lists it");
    let view = page.view(Some(&playing));
    assert!(view.setlists[1].playing);
    assert!(view.editor.unwrap().playing);
}

#[test]
fn leaving_the_library_screen_or_acting_elsewhere_disarms_the_removal() {
    use openvamp_desktop::action::{Intent, Page, UiAction, disarms_removal};

    let (_dir, mut page) = removable("B");
    page.act(LibraryAction::RemoveSetlist);
    page.disarm();
    assert_eq!(remove_label(&page), "Remove");

    // Every intent outside the remove controls disarms.
    for intent in [
        Intent::ShowPage(Page::Perform),
        Intent::ShowPage(Page::Library),
        Intent::Ui(UiAction::TogglePlay),
        Intent::Ui(UiAction::NextSong),
        Intent::PanicDown,
        Intent::PerformSetlist,
        Intent::OpenLibrary,
        Intent::OpenSong,
        Intent::DismissProblems,
        Intent::Library(LibraryAction::AddClick),
    ] {
        assert!(disarms_removal(intent), "{intent:?}");
    }
    assert!(!disarms_removal(Intent::Library(
        LibraryAction::RemoveSetlist
    )));
    assert!(!disarms_removal(Intent::Library(
        LibraryAction::RemoveUnreadable(0)
    )));
}

#[test]
fn a_setlist_file_that_cannot_be_read_is_removed_from_the_list_in_two_presses() {
    let (dir, page) = removable("A");
    drop(page);
    fs::write(
        dir.path().join("Setlists/Broken.setlist.json"),
        "{ not json",
    )
    .unwrap();
    let mut page = LibraryPage::open_with_trash(dir.path(), None, test_trash).unwrap();
    let row = page.view(None).unreadable_setlists[0].clone();
    assert!(
        row.line.starts_with("Broken.setlist.json: "),
        "{}",
        row.line
    );
    assert_eq!(row.remove, "Remove");

    page.act(LibraryAction::RemoveUnreadable(0));
    let row = page.view(None).unreadable_setlists[0].clone();
    assert_eq!(row.remove, ARMED, "the line above names the file");
    assert!(row.remove_armed);
    // The setlist in the editor is not armed by it.
    assert_eq!(remove_label(&page), "Remove");

    page.act(LibraryAction::RemoveUnreadable(0));
    assert_eq!(page.notice(), None);
    assert!(page.view(None).unreadable_setlists.is_empty());
    assert!(dir.path().join("Trash/Broken.setlist.json").exists());
    assert_eq!(setlist_names(&page), ["A", "B", "C"]);
}

/// A trash that takes the file and then leaves the `Setlists` folder
/// unreadable, so listing the setlists again fails after the removal.
#[cfg(unix)]
fn trash_then_lock(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    test_trash(path)?;
    let folder = path.parent().expect("the Setlists folder");
    fs::set_permissions(folder, fs::Permissions::from_mode(0o000))
}

#[cfg(unix)]
#[test]
fn a_removed_setlist_never_stays_open_even_when_the_list_cannot_be_read_again() {
    use std::os::unix::fs::PermissionsExt;

    let dir = library();
    for name in ["A", "B", "C"] {
        save(dir.path(), name, Vec::new());
    }
    let mut page =
        LibraryPage::open_with_trash(dir.path(), Some("B.setlist.json"), trash_then_lock).unwrap();

    page.act(LibraryAction::RemoveSetlist);
    page.act(LibraryAction::RemoveSetlist);
    let folder = dir.path().join("Setlists");
    let locked = fs::read_dir(&folder).is_err();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    if !locked {
        return; // running as root: the folder stays readable
    }

    assert!(dir.path().join("Trash/B.setlist.json").exists());
    assert_ne!(page.selected_file(), Some("B.setlist.json"));
    let view = page.view(None);
    assert!(
        !view.setlists.iter().any(|s| s.name == "B"),
        "{:?}",
        view.setlists
    );
    let warning = view.warning.expect("the failed listing shows as a warning");
    assert!(warning.contains("cannot open folder"), "{warning}");
    // Whatever was selected, no edit can save B again.
    page.act(LibraryAction::AddClick);
    assert!(!folder.join("B.setlist.json").exists());
}

#[test]
fn the_editor_meta_line_leaves_the_arrangement_to_its_column() {
    let dir = library();
    let file = save(
        dir.path(),
        "Sunday",
        vec![
            SetlistItem::Song(SongItem::new("Songs/Bright.song")),
            SetlistItem::Song(SongItem::new("Songs/Gone.song")),
        ],
    );
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();

    let rows = page.view(None).editor.unwrap().rows;
    assert_eq!(rows[0].meta, "90 BPM · 1 stem");
    assert_eq!(rows[0].arrangement, "Default");
    // A song the library lacks has nothing to say but its arrangement,
    // which its column shows.
    assert_eq!(rows[1].meta, "—");
    assert_eq!(rows[1].arrangement, "default");
}

/// Every file of the setlists folder with its bytes.
fn setlist_files(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fs::read_dir(root.join("Setlists"))
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| {
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// A library with one setlist of four items: Amazing, Bright, a click
/// tile at 90 BPM and Amazing again, open in the editor.
fn four_items() -> (tempfile::TempDir, LibraryPage) {
    let dir = library();
    let click = SetlistItem::Click(ClickItem::new(
        90.0,
        TimeSignature {
            numerator: 3,
            denominator: 4,
        },
    ));
    let file = save(
        dir.path(),
        "Sunday",
        vec![
            SetlistItem::Song(SongItem::new("Songs/Amazing.song")),
            SetlistItem::Song(SongItem::new("Songs/Bright.song")),
            click,
            SetlistItem::Song(SongItem {
                fade_out: true,
                ..SongItem::new("Songs/Amazing.song")
            }),
        ],
    );
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    (dir, page)
}

#[test]
fn dropping_an_item_moves_it_as_the_arrows_do_and_saves_it() {
    let (_by_drop_dir, mut by_drop) = four_items();
    let (_by_arrows_dir, mut by_arrows) = four_items();

    by_drop.act(LibraryAction::DropItem { from: 0, to: 2 });
    by_arrows.act(LibraryAction::MoveDown(0));
    by_arrows.act(LibraryAction::MoveDown(1));
    assert_eq!(on_disk(&by_drop).items, on_disk(&by_arrows).items);

    by_drop.act(LibraryAction::DropItem { from: 3, to: 0 });
    for index in [3, 2, 1] {
        by_arrows.act(LibraryAction::MoveUp(index));
    }
    assert_eq!(on_disk(&by_drop).items, on_disk(&by_arrows).items);
    assert_eq!(by_drop.notice(), None);
}

#[test]
fn a_drop_on_itself_or_out_of_the_list_saves_nothing() {
    let (dir, mut page) = four_items();
    let before = setlist_files(dir.path());

    page.act(LibraryAction::DropItem { from: 1, to: 1 });
    page.act(LibraryAction::DropItem { from: 9, to: 0 });
    page.act(LibraryAction::DropItem { from: 0, to: 9 });
    page.act(LibraryAction::DropSetlist { from: 0, to: 0 });
    page.act(LibraryAction::DropSetlist { from: 0, to: 9 });

    assert_eq!(setlist_files(dir.path()), before);
    assert_eq!(page.notice(), None);
}

#[test]
fn dragging_over_a_row_shows_where_the_item_lands_and_the_drop_moves_it_there() {
    let (_dir, mut page) = four_items();
    let insertion = |page: &LibraryPage| page.view(None).editor.unwrap().insertion;
    let titles = |page: &LibraryPage| {
        page.view(None)
            .editor
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row.meta)
            .collect::<Vec<_>>()
    };
    let before = titles(&page);

    page.drag_over(Dragged::Item(0), 2, Half::Lower);
    assert_eq!(insertion(&page), Some(3), "the line goes after row 3");
    // Right after itself: no line, a drop would change nothing.
    page.drag_over(Dragged::Item(0), 0, Half::Lower);
    assert_eq!(insertion(&page), None);
    page.drop_dragged(Dragged::Item(0));
    assert_eq!(titles(&page), before);

    page.drag_over(Dragged::Item(0), 2, Half::Lower);
    page.drop_dragged(Dragged::Item(0));
    assert_eq!(insertion(&page), None, "the line goes when the drag ends");
    assert_eq!(
        titles(&page),
        [&before[1], &before[2], &before[0], &before[3]].map(String::clone)
    );
    on_disk(&page);

    // A drag that ends anywhere else moves nothing.
    let moved = titles(&page);
    page.drag_over(Dragged::Item(3), 0, Half::Upper);
    assert_eq!(insertion(&page), Some(0));
    assert!(page.cancel_drag(), "the line that showed goes");
    assert_eq!(insertion(&page), None);
    assert!(!page.cancel_drag(), "no line showed, nothing to redraw");
    assert_eq!(titles(&page), moved);
    // A setlist dropped while an item's line shows moves nothing either.
    page.drag_over(Dragged::Item(3), 0, Half::Upper);
    page.drop_dragged(Dragged::Setlist(0));
    assert_eq!(titles(&page), moved);
}

#[test]
fn dropping_a_setlist_moves_it_as_the_arrows_do_and_the_order_lasts() {
    let (dir, mut page) = removable("A");

    page.act(LibraryAction::DropSetlist { from: 0, to: 2 });
    assert_eq!(setlist_names(&page), ["B", "C", "A"]);
    assert_eq!(page.selected_file(), Some("A.setlist.json"));

    page.drag_over(Dragged::Setlist(2), 0, Half::Upper);
    assert_eq!(page.view(None).setlist_insertion, Some(0));
    page.drop_dragged(Dragged::Setlist(2));
    assert_eq!(page.view(None).setlist_insertion, None);
    assert_eq!(setlist_names(&page), ["A", "B", "C"]);

    let (_arrows_dir, mut by_arrows) = removable("A");
    by_arrows.act(LibraryAction::MoveSetlistDown(0));
    by_arrows.act(LibraryAction::MoveSetlistDown(1));
    page.act(LibraryAction::DropSetlist { from: 0, to: 2 });
    assert_eq!(setlist_names(&page), setlist_names(&by_arrows));

    let reopened = LibraryPage::open_with_trash(dir.path(), None, test_trash).unwrap();
    assert_eq!(setlist_names(&reopened), ["B", "C", "A"]);
}

#[test]
fn a_drop_refused_by_a_newer_order_file_changes_no_file() {
    let (dir, mut page) = removable("A");
    fs::write(
        dir.path().join("Setlists/order.json"),
        r#"{"formatVersion": 2, "setlists": ["C.setlist.json"]}"#,
    )
    .unwrap();
    let before = setlist_files(dir.path());

    page.act(LibraryAction::DropSetlist { from: 0, to: 2 });

    assert!(
        page.notice()
            .is_some_and(|notice| notice.contains("unsupported formatVersion 2")),
        "{:?}",
        page.notice()
    );
    assert_eq!(setlist_files(dir.path()), before);
}

#[test]
fn a_drag_disarms_the_removal() {
    let (_dir, mut page) = removable("B");
    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(remove_label(&page), ARMED);

    page.start_drag();
    assert_eq!(remove_label(&page), "Remove", "starting a drag disarms");

    page.act(LibraryAction::RemoveSetlist);
    assert_eq!(remove_label(&page), ARMED);
    page.drag_over(Dragged::Setlist(0), 2, Half::Lower);
    assert_eq!(remove_label(&page), "Remove", "dragging over a row disarms");
}

#[test]
fn a_new_drag_dropped_before_it_is_over_a_row_uses_no_old_line() {
    let (dir, mut page) = four_items();
    let insertion = |page: &LibraryPage| page.view(None).editor.unwrap().insertion;
    let files = setlist_files(dir.path());

    // A drag that ended where no drop arrived leaves its line behind; the
    // same row dragged again and dropped at once lands nowhere.
    page.drag_over(Dragged::Item(2), 0, Half::Upper);
    assert_eq!(insertion(&page), Some(0));
    page.start_drag();
    assert_eq!(insertion(&page), None, "a new drag starts with no line");
    page.drop_dragged(Dragged::Item(2));
    assert_eq!(setlist_files(dir.path()), files);

    // A drop for another row than the line was worked out for clears it.
    page.drag_over(Dragged::Item(2), 0, Half::Upper);
    page.drop_dragged(Dragged::Item(1));
    assert_eq!(insertion(&page), None);
    page.drop_dragged(Dragged::Item(2));
    assert_eq!(setlist_files(dir.path()), files);
}

#[test]
fn after_a_drop_the_open_details_stay_on_the_same_item() {
    let selected_item = |page: &LibraryPage| {
        let row = page
            .view(None)
            .editor
            .unwrap()
            .rows
            .iter()
            .position(|row| row.selected)
            .expect("a row is selected");
        on_disk(page).items[row].clone()
    };
    for (selected, from, to) in [
        // The moved item, down and up.
        (0, 0, 3),
        (3, 3, 0),
        // An item it passes, inside the span and at its far end.
        (2, 0, 3),
        (3, 0, 3),
        (1, 3, 0),
        (0, 3, 0),
        // An item outside the span.
        (3, 0, 2),
        (0, 3, 1),
    ] {
        let (_dir, mut page) = four_items();
        page.act(LibraryAction::SelectRow(selected));
        let item = selected_item(&page);
        let before = on_disk(&page).items;

        page.act(LibraryAction::DropItem { from, to });

        assert_ne!(on_disk(&page).items, before, "{from} moved to {to}");
        assert_eq!(
            selected_item(&page),
            item,
            "row {selected} selected, {from} dropped at {to}"
        );
    }
}

/// The editor header's summary of a setlist of `items`.
fn summary_of(items: Vec<SetlistItem>) -> String {
    let dir = library();
    let file = save(dir.path(), "Sunday", items);
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();
    page.view(None).editor.unwrap().summary
}

/// A click-only tile of `bars` bars of 2/4 at 60 BPM: two seconds a bar.
fn two_second_bars(bars: Option<u32>) -> SetlistItem {
    SetlistItem::Click(ClickItem {
        bars,
        ..ClickItem::new(
            60.0,
            TimeSignature {
                numerator: 2,
                denominator: 4,
            },
        )
    })
}

#[test]
fn the_editor_header_counts_the_items_and_rounds_their_length_to_minutes() {
    // As the design writes it: "5 items · ≈ 23 min".
    assert_eq!(
        summary_of(vec![two_second_bars(Some(44))]),
        "1 item · ≈ 1 min"
    );
    assert_eq!(
        summary_of(vec![two_second_bars(Some(45))]),
        "1 item · ≈ 2 min",
        "1:30 rounds up"
    );
    assert_eq!(
        summary_of(vec![
            two_second_bars(Some(600)),
            two_second_bars(Some(77)),
            SetlistItem::Song(SongItem::new("Songs/Amazing.song")),
        ]),
        "3 items · ≈ 23 min",
        "22:38"
    );
    assert_eq!(
        summary_of(vec![SetlistItem::Song(SongItem::new("Songs/Amazing.song"))]),
        "1 item · < 1 min"
    );
    // A length that depends on the operator, or a song the library lacks:
    // the known part, as a floor.
    assert_eq!(
        summary_of(vec![two_second_bars(Some(59)), two_second_bars(None)]),
        "2 items · at least 1 min"
    );
    assert_eq!(
        summary_of(vec![
            SetlistItem::Song(SongItem::new("Songs/Amazing.song")),
            SetlistItem::Song(SongItem::new("Songs/Gone.song")),
        ]),
        "2 items",
        "less than a minute known says nothing about the length"
    );
    // The longest length a file can hold does not overflow.
    assert_eq!(
        summary_of(vec![SetlistItem::Click(ClickItem {
            bars: Some(u32::MAX),
            ..ClickItem::new(
                1.0,
                TimeSignature {
                    numerator: u32::MAX,
                    denominator: 4,
                },
            )
        })]),
        format!("1 item · ≈ {} min", u64::MAX / 60)
    );
    assert_eq!(summary_of(Vec::new()), "No items yet");
}

#[test]
fn the_editor_meta_line_ends_with_the_song_s_stem_count() {
    let dir = library();
    let file = save(
        dir.path(),
        "Sunday",
        vec![
            SetlistItem::Song(SongItem::new("Songs/Amazing.song")),
            SetlistItem::Song(SongItem {
                start_position: Some(1),
                end_position: Some(2),
                fade_out: true,
                ..SongItem::new("Songs/Amazing.song")
            }),
            SetlistItem::Song(SongItem::new("Songs/Gone.song")),
        ],
    );
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();

    let rows = page.view(None).editor.unwrap().rows;
    // As the design writes it: "A · 132 BPM · 14 stems".
    assert_eq!(rows[0].meta, "A · 132 BPM · 1 stem");
    // The excerpt and the fade-out matter more on stage: they come first.
    assert_eq!(
        rows[1].meta,
        "A · 132 BPM · Verse 1 – Chorus · fade-out · 1 stem"
    );
    // A song the library lacks has no stems to count.
    assert_eq!(rows[2].meta, "—");
}

#[test]
fn the_editor_says_in_a_short_line_that_perform_plays_the_setlist_as_opened() {
    let dir = library();
    let file = save(dir.path(), "Sunday", Vec::new());
    let page = LibraryPage::open(dir.path(), Some(&file)).unwrap();

    assert_eq!(
        page.view(Some(&file)).editor.unwrap().on_perform.as_deref(),
        Some("On Perform · changes play the next time it opens")
    );
    assert_eq!(page.view(None).editor.unwrap().on_perform, None);
    assert_eq!(
        page.view(Some("Other.setlist.json"))
            .editor
            .unwrap()
            .on_perform,
        None
    );
}
