//! S4 end to end without a window or hardware: a setlist from a temporary
//! library folder opens in place of a song on the app's `FakeOutput`, moves
//! through its items with the screen's controls, and shows its problems.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use app::testing::FakeOutput;
use app::{AudioOutput, OpenGate, SessionOptions, TransportState};
use domain::{
    Arrangement, ClickItem, SectionMarker, SectionType, Setlist, SetlistItem, SongItem,
    SongPackage, Stem, StemRole, TempoPoint, TimeSignature, TimeSignaturePoint, Transition,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::Library;
use openvamp_desktop::action::UiAction;
use openvamp_desktop::library_page::{LibraryAction, LibraryPage};
use openvamp_desktop::reorder::Half;
use openvamp_desktop::show::{LoadedSetlist, Show, Source, replace, replace_prepared};
use openvamp_desktop::strip::ItemState;
use openvamp_desktop::view::PerformView;

/// A two-bar song (intro and outro, 120 bpm in 4/4) titled `title` at
/// `rate`, with one quiet mono stem.
fn write_song(dir: &Path, title: &str, rate: u32) {
    let bar = 2 * u64::from(rate);
    let section = |id: &str, section_type, start| SectionMarker {
        id: id.to_owned(),
        section_type,
        number: None,
        start,
    };
    let package = SongPackage {
        format_version: 1,
        title: title.to_owned(),
        artist: None,
        sample_rate: rate,
        key: Some("E".to_owned()),
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
            section("intro", SectionType::Intro, 0),
            section("outro", SectionType::Outro, bar),
        ],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["intro".to_owned(), "outro".to_owned()],
            infinite_loop: Vec::new(),
        }],
    };
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(dir.join("keys.wav"), spec).unwrap();
    for frame in 0..2 * bar {
        writer.write_sample((frame % 100) as i16 * 10).unwrap();
    }
    writer.finalize().unwrap();
}

fn song(path: &str) -> SetlistItem {
    SetlistItem::Song(SongItem::new(path))
}

/// Saves a setlist of `items` named `name` in `library` and returns where
/// it is.
fn save_setlist(library: &Path, name: &str, items: Vec<SetlistItem>) -> Source {
    let mut setlist = Setlist::new(name);
    setlist.items = items;
    let entry = Library::new(library).save_setlist(&setlist).unwrap();
    Source::Setlist {
        library: library.to_owned(),
        file: entry.file,
    }
}

fn output(fake: &FakeOutput) -> impl FnMut() -> Box<dyn AudioOutput> + '_ {
    || Box::new(fake.clone())
}

/// A few audio callbacks and screen polls.
fn run(show: &mut Show, fake: &FakeOutput) {
    for _ in 0..8 {
        fake.render(256);
        show.poll();
    }
}

fn setlist_view(show: &Show) -> openvamp_desktop::strip::SetlistView {
    show.setlist().expect("a setlist plays").view()
}

#[test]
fn a_setlist_opens_in_place_of_a_song_and_moves_through_its_items() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song(&library.join("Songs/One.song"), "First", 8_000);
    write_song(&library.join("Songs/Two.song"), "Second", 8_000);
    let source = save_setlist(
        library,
        "Sunday",
        vec![
            song("Songs/One.song"),
            song("Songs/Missing.song"),
            song("Songs/Two.song"),
            SetlistItem::Click(ClickItem {
                bars: Some(2),
                ..ClickItem::new(
                    90.0,
                    TimeSignature {
                        numerator: 3,
                        denominator: 4,
                    },
                )
            }),
        ],
    );
    let fake = FakeOutput::new();
    let options = SessionOptions::default();
    let mut current = None;
    replace(
        &mut current,
        &Source::Song(library.join("Songs/Two.song")),
        &options,
        output(&fake),
        |_| {},
    )
    .unwrap();
    assert!(matches!(current, Some(Show::Song(_))));

    let replaced = replace(&mut current, &source, &options, output(&fake), |_| {}).unwrap();

    assert_eq!(replaced.problems(), Vec::<String>::new());
    let (closed, previous) = replaced.previous.as_ref().unwrap();
    assert_eq!(closed.title, "Second");
    assert_eq!(previous, &Source::Song(library.join("Songs/Two.song")));
    let show = current.as_mut().unwrap();
    run(show, &fake);
    let view = setlist_view(show);
    assert_eq!(view.name, "Sunday");
    assert_eq!(view.position, "1 of 4");
    let titles: Vec<&str> = view.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["First", "Missing", "Second", "Click"]);
    assert_eq!(view.items[0].meta, "E · 120 BPM · Default");
    assert_eq!(view.items[3].meta, "90 BPM · 3/4 · 2 bars");
    assert_eq!(view.items[0].state, ItemState::Current);
    assert!(view.items[2].is_next, "the missing item is skipped");
    assert!(!view.items[1].playable);
    assert!(
        view.problems[0].starts_with("Item 2 \"Missing\" cannot play: song \"Songs/Missing.song\""),
        "{:?}",
        view.problems
    );
    let perform = PerformView::new(show.snapshot(), show.song());
    assert_eq!(perform.title, "First");

    // Nothing comes before the first item: the refusal shows until a
    // control is accepted.
    show.send(UiAction::PreviousSong);
    run(show, &fake);
    assert_eq!(
        setlist_view(show).problems.last().map(String::as_str),
        Some("Refused: no item before the current one can play")
    );

    show.send(UiAction::NextSong);
    run(show, &fake);
    let view = setlist_view(show);
    assert_eq!(view.position, "3 of 4");
    assert_eq!(view.items[0].state, ItemState::Played);
    assert_eq!(view.items[2].state, ItemState::Current);
    assert!(!view.problems.iter().any(|line| line.starts_with("Refused")));
    assert_eq!(
        PerformView::new(show.snapshot(), show.song()).title,
        "Second"
    );

    // A tile selects its item; back to start acts on the current item.
    show.send(UiAction::SelectItem(3));
    run(show, &fake);
    assert_eq!(setlist_view(show).position, "4 of 4");
    assert_eq!(show.song().title, "Click");
    show.send(UiAction::BackToStart);
    run(show, &fake);
    assert_eq!(show.snapshot().last_error, None);

    // A song opens in place of the setlist again.
    let replaced = replace(
        &mut current,
        &Source::Song(library.join("Songs/One.song")),
        &options,
        output(&fake),
        |_| {},
    )
    .unwrap();
    assert_eq!(replaced.previous.as_ref().unwrap().0.title, "Sunday");
    assert!(matches!(current, Some(Show::Song(_))));
}

#[test]
fn the_device_held_for_another_rate_shows_in_the_setlist_problems() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song(&library.join("Low.song"), "Low", 8_000);
    write_song(&library.join("High.song"), "High", 16_000);
    let source = save_setlist(library, "Rates", vec![song("Low.song"), song("High.song")]);
    let fake = FakeOutput::without_devices().with_device("Speakers", 2, Some(&[8_000]));
    let mut current = None;
    replace(
        &mut current,
        &source,
        &SessionOptions {
            reconnect_interval: Duration::from_millis(1),
            ..SessionOptions::default()
        },
        output(&fake),
        |_| {},
    )
    .unwrap();
    let show = current.as_mut().unwrap();
    run(show, &fake);
    assert!(
        setlist_view(show)
            .problems
            .contains(&"Item 2 \"High\" runs at 16000 Hz and the device at 8000 Hz: it plays once the device reopens at its rate, and no AutoLink or Crossfade leads into it".to_owned())
    );

    show.send(UiAction::NextSong);
    for _ in 0..20 {
        run(show, &fake);
    }

    let problems = setlist_view(show).problems;
    let held = problems.last().unwrap();
    assert!(
        held.starts_with("The device is held for item 2 \"High\" at 16000 Hz, which it refuses ("),
        "{problems:?}"
    );
    assert!(
        held.ends_with("choose an item at 8000 Hz to play again"),
        "{held}"
    );

    // Selecting an item at the device's rate brings it back.
    show.send(UiAction::PreviousSong);
    for _ in 0..20 {
        run(show, &fake);
    }
    let problems = setlist_view(show).problems;
    assert!(
        !problems
            .iter()
            .any(|line| line.starts_with("The device is held")),
        "{problems:?}"
    );
}

#[test]
fn a_setlist_with_nothing_to_play_leaves_the_song_playing_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song(&library.join("One.song"), "First", 8_000);
    let source = save_setlist(library, "Empty", vec![song("Gone.song")]);
    let fake = FakeOutput::new();
    let options = SessionOptions::default();
    let mut current = None;
    replace(
        &mut current,
        &Source::Song(library.join("One.song")),
        &options,
        output(&fake),
        |_| {},
    )
    .unwrap();

    let error = replace(&mut current, &source, &options, output(&fake), |_| {}).unwrap_err();

    let lines = error.lines();
    assert_eq!(lines[0], "setlist \"Empty\" has no item that can play");
    assert!(
        lines[1].starts_with("item 1 cannot play: song \"Gone.song\""),
        "{lines:?}"
    );
    assert!(
        matches!(current, Some(Show::Song(_))),
        "the song still plays"
    );
}

#[test]
fn a_playing_setlist_follows_its_file_when_it_moves() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song(&library.join("One.song"), "First", 8_000);
    let source = save_setlist(library, "Sunday", vec![song("One.song")]);
    let fake = FakeOutput::new();
    let mut current = None;
    replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        output(&fake),
        |_| {},
    )
    .unwrap();
    let show = current.as_mut().unwrap();
    // The library is kept canonical, the form the Library screen gives.
    let library = &std::fs::canonicalize(library).unwrap();

    show.follow_move(library, "Sunday.setlist.json", "Zed.setlist.json");
    assert_eq!(
        show.source(),
        Source::Setlist {
            library: library.to_owned(),
            file: "Zed.setlist.json".to_owned(),
        }
    );
    // A move in another library, or of another file, changes nothing.
    show.follow_move(
        Path::new("/elsewhere"),
        "Zed.setlist.json",
        "Other.setlist.json",
    );
    show.follow_move(library, "Other.setlist.json", "Third.setlist.json");
    assert_eq!(
        show.source(),
        Source::Setlist {
            library: library.to_owned(),
            file: "Zed.setlist.json".to_owned(),
        }
    );
}

#[test]
fn a_setlist_keeps_its_library_folder_canonical() {
    let dir = tempfile::tempdir().unwrap();
    write_song(&dir.path().join("One.song"), "First", 8_000);
    save_setlist(dir.path(), "Sunday", vec![song("One.song")]);
    let fake = FakeOutput::new();
    let mut current = None;
    let source = Source::Setlist {
        library: dir.path().join("One.song/.."),
        file: "Sunday.setlist.json".to_owned(),
    };
    replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        output(&fake),
        |_| {},
    )
    .unwrap();

    assert_eq!(
        current.unwrap().source(),
        Source::Setlist {
            library: std::fs::canonicalize(dir.path()).unwrap(),
            file: "Sunday.setlist.json".to_owned(),
        }
    );
}

#[test]
fn a_refused_control_is_forgotten_once_the_setlist_moves_on_by_itself() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song(&library.join("One.song"), "First", 8_000);
    write_song(&library.join("Two.song"), "Second", 8_000);
    let mut first = SongItem::new("One.song");
    first.transition = domain::Transition::AutoLink;
    let source = save_setlist(
        library,
        "Sunday",
        vec![SetlistItem::Song(first), song("Two.song")],
    );
    let fake = FakeOutput::new();
    let mut current = None;
    replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        output(&fake),
        |_| {},
    )
    .unwrap();
    let show = current.as_mut().unwrap();
    show.send(UiAction::PreviousSong);
    run(show, &fake);
    assert!(
        setlist_view(show)
            .problems
            .iter()
            .any(|l| l.starts_with("Refused"))
    );

    show.send(UiAction::TogglePlay);
    for _ in 0..2_000 {
        if setlist_view(show).position == "2 of 2" {
            break;
        }
        for monitor in show.setlist().unwrap().session.disk_monitors() {
            assert!(monitor.wait_idle(Duration::from_secs(10)));
        }
        fake.render(256);
        show.poll();
    }

    let view = setlist_view(show);
    assert_eq!(view.position, "2 of 2", "AutoLink moved on");
    assert!(
        !view.problems.iter().any(|l| l.starts_with("Refused")),
        "{:?}",
        view.problems
    );
}

#[cfg(unix)]
#[test]
fn songs_the_library_cannot_list_show_as_a_note_and_items_keep_their_folder_names() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("Library");
    write_song(&library.join("One.song"), "First", 8_000);
    let source = save_setlist(&library, "Sunday", vec![song("One.song")]);
    // Searchable but not readable: paths inside resolve, the folder does
    // not list.
    std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o311)).unwrap();
    let listable = std::fs::read_dir(&library).is_ok();
    let fake = FakeOutput::new();
    let mut current = None;
    let opened = replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        output(&fake),
        |_| {},
    );
    std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o755)).unwrap();
    if listable {
        // Running as root: the folder stays readable.
        return;
    }
    opened.unwrap();

    let view = setlist_view(current.as_ref().unwrap());
    assert_eq!(view.items[0].title, "One");
    assert!(
        view.problems.iter().any(|line| line.starts_with(
            "The library's songs could not be listed, so items show their folder names: "
        )),
        "{:?}",
        view.problems
    );
}

/// A device that is checked fine but held by another song, so opening a
/// pick on it fails after the check.
fn busy_device(dir: &Path) -> (FakeOutput, Option<Show>) {
    let busy = FakeOutput::new();
    write_song(&dir.join("Elsewhere.song"), "Elsewhere", 8_000);
    let mut holder = None;
    replace(
        &mut holder,
        &Source::Song(dir.join("Elsewhere.song")),
        &SessionOptions::default(),
        output(&busy),
        |_| {},
    )
    .unwrap();
    (busy, holder)
}

#[test]
fn a_setlist_comes_back_stopped_when_the_pick_cannot_start_on_its_device() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("Library");
    write_song(&library.join("One.song"), "First", 8_000);
    write_song(&dir.path().join("Pick.song"), "Pick", 8_000);
    let source = save_setlist(&library, "Sunday", vec![song("One.song")]);
    let device = FakeOutput::new();
    let mut current = None;
    replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        output(&device),
        |_| {},
    )
    .unwrap();
    let (busy, _holder) = busy_device(&dir.path().join("other"));

    let calls = std::cell::Cell::new(0);
    let replaced = replace(
        &mut current,
        &Source::Song(dir.path().join("Pick.song")),
        &SessionOptions::default(),
        || {
            calls.set(calls.get() + 1);
            let output = if calls.get() == 1 { &busy } else { &device };
            Box::new(output.clone()) as Box<dyn AudioOutput>
        },
        |_| {
            device.render(256);
        },
    )
    .expect("the check lets the pick go ahead");

    assert!(replaced.start_error.is_some());
    assert!(replaced.reopen_error.is_none());
    let show = current.as_mut().expect("the setlist is back");
    assert_eq!(setlist_view(show).name, "Sunday");
    assert_eq!(show.snapshot().transport, app::TransportState::Stopped);
    let problems = replaced.problems();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!(
            "Could not start {} on the audio device: ",
            dir.path().join("Pick.song").display()
        )),
        "{problems:?}"
    );
    assert_eq!(
        problems[1],
        "\"Sunday\" is loaded again, stopped at the start."
    );
}

#[test]
fn when_the_setlist_cannot_come_back_nothing_is_loaded_and_both_reasons_show() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("Library");
    write_song(&library.join("One.song"), "First", 8_000);
    write_song(&dir.path().join("Pick.song"), "Pick", 8_000);
    let source = save_setlist(&library, "Sunday", vec![song("One.song")]);
    let device = FakeOutput::new();
    let mut current = None;
    replace(
        &mut current,
        &source,
        &SessionOptions::default(),
        output(&device),
        |_| {},
    )
    .unwrap();
    let (busy, _holder) = busy_device(&dir.path().join("other"));

    let replaced = replace(
        &mut current,
        &Source::Song(dir.path().join("Pick.song")),
        &SessionOptions::default(),
        || Box::new(busy.clone()) as Box<dyn AudioOutput>,
        |_| {
            device.render(256);
        },
    )
    .unwrap();

    assert!(current.is_none());
    let problems = replaced.problems();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[1].starts_with("\"Sunday\" could not be loaded again: "),
        "{problems:?}"
    );
}

// L46: dragging the Perform strip reorders the setlist while it plays.

/// Songs One, Two and Three, titled First, Second and Third, in the setlist
/// "Sunday" (One links into Two), open on `fake`.
fn three_songs(fake: &FakeOutput) -> (tempfile::TempDir, LoadedSetlist) {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    for (folder, title) in [("One", "First"), ("Two", "Second"), ("Three", "Third")] {
        write_song(&library.join(format!("Songs/{folder}.song")), title, 8_000);
    }
    let one = SetlistItem::Song(SongItem {
        transition: Transition::AutoLink,
        ..SongItem::new("Songs/One.song")
    });
    let items = vec![one, song("Songs/Two.song"), song("Songs/Three.song")];
    let Source::Setlist { file, .. } = save_setlist(library, "Sunday", items) else {
        unreachable!("a setlist is saved");
    };
    let options = SessionOptions::default();
    let loaded = LoadedSetlist::open(library, &file, &options, Box::new(fake.clone()))
        .unwrap_or_else(|_| panic!("the setlist opens"));
    (dir, loaded)
}

fn strip_titles(loaded: &LoadedSetlist) -> Vec<String> {
    loaded
        .view()
        .items
        .into_iter()
        .map(|item| item.title)
        .collect()
}

/// The songs of the setlist `file` of `library`, as its file holds them.
fn songs_on_disk(library: &Path, file: &str) -> Vec<String> {
    Library::new(library)
        .load_setlist(file)
        .unwrap()
        .items
        .into_iter()
        .map(|item| match item {
            SetlistItem::Song(song) => song.song,
            SetlistItem::Click(_) => "click".to_owned(),
        })
        .collect()
}

fn setlist_bytes(library: &Path, file: &str) -> Vec<u8> {
    fs::read(library.join(library::SETLISTS_FOLDER).join(file)).unwrap()
}

#[test]
fn a_tile_dropped_along_the_strip_moves_live_and_its_file_keeps_the_new_order() {
    let fake = FakeOutput::new();
    let (dir, mut loaded) = three_songs(&fake);
    assert_eq!(
        strip_titles(&loaded),
        ["First", "Second", "Third"],
        "every tile has its title"
    );

    loaded.start_drag();
    loaded.drag_over(2, 0, Half::Upper);
    assert_eq!(
        loaded.view().insertion,
        Some(0),
        "the line shows before First"
    );
    loaded.drop_dragged(2);

    let view = loaded.view();
    assert_eq!(view.insertion, None);
    assert_eq!(strip_titles(&loaded), ["Third", "First", "Second"]);
    assert_eq!(view.items[1].state, ItemState::Current, "First plays on");
    assert_eq!(view.problems, Vec::<String>::new());
    let file = loaded.file.clone();
    assert_eq!(
        songs_on_disk(dir.path(), &file),
        ["Songs/Three.song", "Songs/One.song", "Songs/Two.song"]
    );
    assert_eq!(loaded.take_saves(), [(file.clone(), file)]);
    assert_eq!(loaded.take_saves(), []);
}

#[test]
fn a_drop_on_itself_or_with_no_line_of_its_own_moves_and_saves_nothing() {
    let fake = FakeOutput::new();
    let (dir, mut loaded) = three_songs(&fake);
    let bytes = setlist_bytes(dir.path(), &loaded.file);

    // On itself: no line, nothing moves.
    loaded.drag_over(1, 1, Half::Lower);
    assert_eq!(loaded.view().insertion, None);
    loaded.drop_dragged(1);
    // A line a drag that never dropped left behind is gone once the next
    // drag starts.
    loaded.drag_over(1, 2, Half::Lower);
    assert_eq!(loaded.view().insertion, Some(3));
    loaded.start_drag();
    assert_eq!(loaded.view().insertion, None);
    loaded.drop_dragged(1);
    // A drop for another tile than the line was worked out for.
    loaded.drag_over(1, 2, Half::Lower);
    loaded.drop_dragged(0);
    assert_eq!(loaded.view().insertion, None);
    // Dropped outside the strip.
    loaded.drag_over(1, 0, Half::Upper);
    assert!(loaded.cancel_drag());
    assert!(!loaded.cancel_drag());

    assert_eq!(strip_titles(&loaded), ["First", "Second", "Third"]);
    assert_eq!(setlist_bytes(dir.path(), &loaded.file), bytes);
    assert_eq!(loaded.take_saves(), []);
}

/// Waits until the disk threads of `loaded` are idle.
fn wait_disks(loaded: &LoadedSetlist) {
    for monitor in loaded.session.disk_monitors() {
        assert!(monitor.wait_idle(Duration::from_secs(5)));
    }
}

#[test]
fn a_move_during_a_hand_over_is_refused_with_a_notice_until_it_ends_and_saves_nothing() {
    let fake = FakeOutput::new();
    let (dir, mut loaded) = three_songs(&fake);
    for _ in 0..200 {
        wait_disks(&loaded);
        loaded.poll();
        if loaded.snapshot.setlist.next_ready {
            break;
        }
    }
    assert!(loaded.snapshot.setlist.next_ready, "Second is cued");
    loaded.send(UiAction::TogglePlay);
    for _ in 0..100 {
        wait_disks(&loaded);
        fake.render(256);
        loaded.poll();
        if loaded.snapshot.song.output_frame > 0 {
            break;
        }
    }
    // Rendered with no poll past First's end: the deck hands over to
    // Second on its own.
    for _ in 0..32_000 / 256 + 8 {
        wait_disks(&loaded);
        fake.render(256);
    }
    let bytes = setlist_bytes(dir.path(), &loaded.file);

    loaded.move_item(2, 0);

    let problems = loaded.view().problems;
    assert_eq!(
        problems,
        ["Refused: the setlist is moving on to the next item; it can be reordered once it has"]
    );
    assert_eq!(strip_titles(&loaded), ["First", "Second", "Third"]);
    assert_eq!(setlist_bytes(dir.path(), &loaded.file), bytes);
    assert_eq!(loaded.take_saves(), []);
    loaded.poll();
    assert_eq!(loaded.snapshot.setlist.current, 1, "Second plays");
    assert_eq!(
        loaded.view().problems,
        Vec::<String>::new(),
        "the notice goes"
    );
    loaded.move_item(2, 0);
    assert_eq!(strip_titles(&loaded), ["Third", "First", "Second"]);
}

#[test]
fn a_file_changed_since_the_setlist_opened_keeps_the_new_order_playing_and_says_it_was_not_saved() {
    let fake = FakeOutput::new();
    let (dir, mut loaded) = three_songs(&fake);
    // The Library screen took Third out of the file since.
    let library = Library::new(dir.path());
    let mut setlist = library.load_setlist(&loaded.file).unwrap();
    setlist.items.pop();
    library.save_setlist(&setlist).unwrap();
    let bytes = setlist_bytes(dir.path(), &loaded.file);

    loaded.drag_over(0, 1, Half::Lower);
    loaded.drop_dragged(0);

    let not_saved = [format!(
        "The new order plays but was not saved to {}: the file changed since the setlist was opened; open it again to play those changes",
        loaded.file
    )];
    assert_eq!(strip_titles(&loaded), ["Second", "First", "Third"]);
    assert_eq!(setlist_bytes(dir.path(), &loaded.file), bytes);
    assert_eq!(loaded.view().problems, not_saved);
    assert_eq!(loaded.take_saves(), []);

    // Every later move plays and keeps away from the changed file.
    loaded.move_item(2, 0);
    assert_eq!(strip_titles(&loaded), ["Third", "Second", "First"]);
    assert_eq!(setlist_bytes(dir.path(), &loaded.file), bytes);
    assert_eq!(loaded.view().problems, not_saved);
    assert_eq!(loaded.take_saves(), []);
}

#[cfg(unix)]
#[test]
fn a_move_that_could_not_be_saved_is_saved_with_the_next_one() {
    use std::os::unix::fs::PermissionsExt;

    let fake = FakeOutput::new();
    let (dir, mut loaded) = three_songs(&fake);
    let folder = dir.path().join(library::SETLISTS_FOLDER);
    let bytes = setlist_bytes(dir.path(), &loaded.file);
    let mode = |mode| fs::Permissions::from_mode(mode);

    // The setlists folder takes no file for a moment.
    fs::set_permissions(&folder, mode(0o555)).unwrap();
    loaded.move_item(2, 0);
    fs::set_permissions(&folder, mode(0o755)).unwrap();

    assert_eq!(strip_titles(&loaded), ["Third", "First", "Second"]);
    assert_eq!(setlist_bytes(dir.path(), &loaded.file), bytes);
    let problems = loaded.view().problems;
    let not_saved = format!("The new order plays but was not saved to {}: ", loaded.file);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with(&not_saved), "{problems:?}");
    assert!(!problems[0].contains("changed since"), "{problems:?}");
    assert_eq!(loaded.take_saves(), []);

    // The file still holds the order it was last known to hold, so the
    // next move saves the whole order playing.
    loaded.move_item(2, 1);

    assert_eq!(strip_titles(&loaded), ["Third", "Second", "First"]);
    assert_eq!(loaded.view().problems, Vec::<String>::new());
    assert_eq!(
        songs_on_disk(dir.path(), &loaded.file),
        ["Songs/Three.song", "Songs/Two.song", "Songs/One.song"]
    );
    let file = loaded.file.clone();
    assert_eq!(loaded.take_saves(), [(file.clone(), file)]);
}

#[test]
fn a_setlist_in_a_file_named_by_hand_follows_its_file_when_a_drop_saves_it() {
    let fake = FakeOutput::new();
    let (dir, loaded) = three_songs(&fake);
    let named = loaded.file.clone();
    drop(loaded);
    let folder = dir.path().join(library::SETLISTS_FOLDER);
    let by_hand = format!("By hand{}", library::SETLIST_EXTENSION);
    fs::rename(folder.join(&named), folder.join(&by_hand)).unwrap();
    let options = SessionOptions::default();
    let mut loaded = LoadedSetlist::open(dir.path(), &by_hand, &options, Box::new(fake.clone()))
        .unwrap_or_else(|_| panic!("the setlist opens"));

    loaded.drag_over(2, 0, Half::Upper);
    loaded.drop_dragged(2);

    assert_eq!(loaded.file, named, "it moved to the file its name maps to");
    assert!(!folder.join(&by_hand).exists());
    assert_eq!(
        songs_on_disk(dir.path(), &named),
        ["Songs/Three.song", "Songs/One.song", "Songs/Two.song"]
    );
    assert_eq!(loaded.take_saves(), [(by_hand, named)]);
}

#[test]
fn the_library_screen_reads_a_setlist_saved_from_the_strip_again() {
    let fake = FakeOutput::new();
    let (dir, mut loaded) = three_songs(&fake);
    let mut page = LibraryPage::open(dir.path(), Some(&loaded.file)).unwrap();

    loaded.drag_over(2, 0, Half::Upper);
    loaded.drop_dragged(2);
    for (from, to) in loaded.take_saves() {
        page.setlist_saved(&from, &to);
    }

    let rows: Vec<String> = page
        .view(None)
        .editor
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.title)
        .collect();
    assert_eq!(rows, ["Third", "First", "Second"]);
    // An edit there starts from the order saved, not the one it read first.
    page.act(LibraryAction::MoveDown(0));
    assert_eq!(
        songs_on_disk(dir.path(), &loaded.file),
        ["Songs/One.song", "Songs/Three.song", "Songs/Two.song"]
    );
}

#[test]
fn an_order_of_other_items_than_the_setlist_holds_is_refused() {
    use openvamp_desktop::editor::{Edit, EditError, SetlistEditor};

    let (dir, loaded) = three_songs(&FakeOutput::new());
    let mut editor = SetlistEditor::open(Library::new(dir.path()), &loaded.file).unwrap();
    let held = editor.setlist().items.clone();
    let bytes = setlist_bytes(dir.path(), &loaded.file);

    let mut fewer = held.clone();
    fewer.pop();
    let mut other = held.clone();
    other[0] = held[1].clone();
    for items in [fewer, other] {
        assert!(matches!(
            editor.apply(Edit::Order(items)),
            Err(EditError::NotTheSameItems)
        ));
    }
    assert_eq!(setlist_bytes(dir.path(), &loaded.file), bytes);

    let mut rotated = held;
    rotated.rotate_left(1);
    assert_eq!(editor.apply(Edit::Order(rotated)).unwrap(), None);
    assert_eq!(
        songs_on_disk(dir.path(), &loaded.file),
        ["Songs/Two.song", "Songs/Three.song", "Songs/One.song"]
    );
}

/// How many opens a [`held_open`] gate holds right now, and whether it was
/// released; with the condition it signals.
type Valve = Arc<(Mutex<(usize, bool)>, Condvar)>;

/// Holds every open of a song folder ending in `held`, as a volume that does
/// not answer would, until the returned flag is set (at most ten seconds);
/// the count says how many opens are held right now.
fn held_open(held: &'static str) -> (OpenGate, Valve) {
    let valve = Arc::new((Mutex::new((0, false)), Condvar::new()));
    let state = Arc::clone(&valve);
    let gate: OpenGate = Arc::new(move |dir: &Path| {
        if !dir.ends_with(held) {
            return;
        }
        let (lock, changed) = &*state;
        let mut valve = lock.lock().unwrap();
        valve.0 += 1;
        changed.notify_all();
        let (mut valve, _) = changed
            .wait_timeout_while(valve, Duration::from_secs(10), |valve| !valve.1)
            .unwrap();
        valve.0 -= 1;
    });
    (gate, valve)
}

#[test]
fn a_pick_prepares_away_from_the_screen_while_the_show_plays_on_and_takes_panic() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song(&library.join("Songs/One.song"), "First", 8_000);
    write_song(&library.join("Songs/Two.song"), "Second", 8_000);
    let fake = FakeOutput::new();
    let mut current = None;
    replace(
        &mut current,
        &Source::Song(library.join("Songs/One.song")),
        &SessionOptions::default(),
        output(&fake),
        |_| {},
    )
    .unwrap();
    let show = current.as_mut().unwrap();
    show.send(UiAction::TogglePlay);
    for _ in 0..1_000 {
        if show.snapshot().transport == TransportState::Playing {
            break;
        }
        thread::sleep(Duration::from_millis(1));
        fake.render(256);
        show.poll();
    }
    assert_eq!(show.snapshot().transport, TransportState::Playing);

    // A setlist is picked whose first song's volume does not answer: it
    // prepares on another thread, as the screen prepares it on its
    // background executor.
    let pick = save_setlist(library, "Sunday", vec![song("Songs/Two.song")]);
    let (gate, valve) = held_open("Two.song");
    let options = SessionOptions {
        open_gate: Some(gate),
        ..SessionOptions::default()
    };
    let preparing = thread::spawn({
        let pick = pick.clone();
        move || pick.prepare(&options)
    });
    {
        let (lock, changed) = &*valve;
        let held = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |v| v.0 == 0)
            .unwrap()
            .0;
        assert_eq!(held.0, 1, "the pick's read is held");
    }

    // Meanwhile what plays polls and takes Panic at once.
    let started = Instant::now();
    show.send(UiAction::Panic);
    for _ in 0..40 {
        fake.render(256);
        show.poll();
    }
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(show.snapshot().transport, TransportState::Stopped);
    assert_eq!(show.song().title, "First");
    assert_eq!(valve.0.lock().unwrap().0, 1, "the read still hangs");

    // The read comes back: the pick takes the place of what played.
    valve.0.lock().unwrap().1 = true;
    valve.1.notify_all();
    let prepared = preparing.join().unwrap().expect("the setlist prepares");
    let replaced = replace_prepared(
        &mut current,
        &pick,
        prepared,
        Box::new(fake.clone()),
        |_| {},
    )
    .unwrap();
    assert_eq!(replaced.problems(), Vec::<String>::new());
    assert_eq!(replaced.previous.as_ref().unwrap().0.title, "First");
    let show = current.as_mut().unwrap();
    assert_eq!(show.song().title, "Second");
    assert_eq!(setlist_view(show).name, "Sunday");
}
