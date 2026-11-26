//! S4: the Perform screen's setlist strip, its controls and its problems,
//! without a window: one test per rule.

use std::path::Path;
use std::time::Duration;

use app::{
    BlendView, Command, DeviceStatus, FadeState, FadeView, ItemInfo, JumpTiming, LossReason,
    MixerView, OutputError, PositionInfo, RateFallback, RuntimeError, SetlistCommand,
    SetlistCommandError, SetlistInfo, SetlistProblem, SetlistSnapshot, SetlistState, Snapshot,
    SongInfo, StreamInfo, TransportState,
};
use domain::{ClickItem, SetlistItem, SongItem, TimeSignature, Transition};
use library::{ArrangementEntry, PositionEntry, SongEntry};
use openvamp_desktop::action::{UiAction, command_for, setlist_command_for};
use openvamp_desktop::catalog::Catalog;
use openvamp_desktop::keys::{KeyAction, KeyPress, Mods, key_down};
use openvamp_desktop::show::Source;
use openvamp_desktop::strip::{ItemState, SetlistView, StripItem, TransitionChip};

const RATE: u32 = 48_000;

fn arrangement(id: &str, name: &str, labels: &[&str]) -> ArrangementEntry {
    ArrangementEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        positions: labels
            .iter()
            .map(|label| PositionEntry {
                section_id: label.to_lowercase(),
                label: (*label).to_owned(),
                length_frames: 96_000,
            })
            .collect(),
    }
}

fn entry(path: &str, title: &str, key: Option<&str>, bpm: f64, rate: u32) -> SongEntry {
    SongEntry {
        song: path.to_owned(),
        title: title.to_owned(),
        artist: None,
        key: key.map(str::to_owned),
        bpm,
        sample_rate: rate,
        stems: 4,
        length_frames: 480_000,
        arrangements: vec![
            arrangement(
                "default",
                "Default",
                &["Intro", "Verse 1", "Chorus", "Outro"],
            ),
            arrangement(
                "long",
                "Long ending",
                &["Intro", "Verse 1", "Chorus", "Tag"],
            ),
        ],
    }
}

fn catalog() -> Catalog {
    Catalog::new(vec![
        entry("First Light.song", "First Light", Some("G"), 104.0, RATE),
        entry("Hymns/Gates.song", "Open the Gates", Some("A"), 132.0, RATE),
        entry("Rivers.song", "Rivers Rise", None, 128.0, 44_100),
    ])
}

fn song_item(path: &str, transition: Transition) -> SongItem {
    SongItem {
        transition,
        ..SongItem::new(path)
    }
}

fn playable(item: SetlistItem, sample_rate: Option<u32>) -> ItemInfo {
    ItemInfo {
        item,
        sample_rate,
        problems: Vec::new(),
        fallback: None,
    }
}

const MISSING: &str = "song \"Missing.song\" cannot be loaded: cannot read song.json";

/// Five items: AutoLink, Crossfade with an excerpt and a fade-out, a
/// missing song, a click tile that stays, and a song at another rate.
fn info() -> SetlistInfo {
    SetlistInfo {
        name: "Sunday 9:00".to_owned(),
        sample_rate: RATE,
        items: vec![
            playable(
                SetlistItem::Song(song_item("Hymns/Gates.song", Transition::AutoLink)),
                Some(RATE),
            ),
            playable(
                SetlistItem::Song(SongItem {
                    arrangement: "long".to_owned(),
                    start_position: Some(1),
                    end_position: Some(2),
                    fade_out: true,
                    ..song_item("First Light.song", Transition::Crossfade)
                }),
                Some(RATE),
            ),
            ItemInfo {
                item: SetlistItem::Song(song_item("Missing.song", Transition::CueNext)),
                sample_rate: None,
                problems: vec![MISSING.to_owned()],
                fallback: None,
            },
            playable(
                SetlistItem::Click(ClickItem {
                    bars: Some(8),
                    transition: Transition::Stay,
                    ..ClickItem::new(
                        70.0,
                        TimeSignature {
                            numerator: 6,
                            denominator: 8,
                        },
                    )
                }),
                None,
            ),
            playable(
                SetlistItem::Song(song_item("Rivers.song", Transition::CueNext)),
                Some(44_100),
            ),
        ],
    }
}

fn song_snapshot() -> Snapshot {
    Snapshot {
        transport: TransportState::Playing,
        output_frame: 0,
        source_frame: 0,
        bar: 1,
        beat: 1,
        position: 0,
        section: "Verse 1".to_owned(),
        queued: None,
        jump_timing: JumpTiming::SectionEnd,
        infinite: false,
        fade: FadeView {
            state: FadeState::In,
            level: 1.0,
            duration: Duration::from_secs(3),
        },
        device: DeviceStatus::Connected(StreamInfo {
            device: "Speakers".to_owned(),
            device_id: "host:speakers".to_owned(),
            sample_rate: RATE,
            buffer_frames: 256,
            channels: 2,
        }),
        mixer: MixerView::default(),
        underruns: 0,
        decode_errors: 0,
        last_error: None,
    }
}

/// Item 2 plays; the missing item 3 is skipped, so the click tile is next,
/// not buffered yet.
fn snapshot() -> SetlistSnapshot {
    SetlistSnapshot {
        song: song_snapshot(),
        setlist: SetlistState {
            current: 1,
            next: Some(3),
            transition: Transition::Crossfade,
            fallback: None,
            next_ready: false,
            blend: None,
            problem: None,
        },
    }
}

fn chip(transition: Transition, short: &'static str, name: &str) -> Option<TransitionChip> {
    Some(TransitionChip {
        transition,
        written: transition,
        short,
        label: format!("Transition: {name}"),
    })
}

const OTHER_RATE: &str = "Item 5 \"Rivers Rise\" runs at 44100 Hz and the device at 48000 Hz: it plays once the device reopens at its rate, and no AutoLink or Crossfade leads into it";

#[test]
fn the_strip_shows_every_item_with_the_current_and_next_one_and_each_transition() {
    let view = SetlistView::new(&info(), &snapshot(), &catalog(), None);

    assert_eq!(view.name, "Sunday 9:00");
    assert_eq!(view.position, "2 of 5");
    assert!(view.has_previous && view.has_next);
    assert_eq!(view.blend, None);
    assert_eq!(
        view.items,
        [
            StripItem {
                index: 0,
                number: "1".to_owned(),
                title: "Open the Gates".to_owned(),
                meta: "A · 132 BPM · Default".to_owned(),
                click: false,
                state: ItemState::Played,
                is_next: false,
                loading: false,
                warning: None,
                playable: true,
                transition: chip(Transition::AutoLink, "Auto", "AutoLink"),
            },
            StripItem {
                index: 1,
                number: "2".to_owned(),
                title: "First Light".to_owned(),
                meta: "G · 104 BPM · Long ending · Verse 1 – Chorus · fade-out".to_owned(),
                click: false,
                state: ItemState::Current,
                is_next: false,
                loading: false,
                warning: None,
                playable: true,
                transition: chip(Transition::Crossfade, "X-fade", "Crossfade"),
            },
            StripItem {
                index: 2,
                number: "3".to_owned(),
                title: "Missing".to_owned(),
                meta: "arrangement \"default\"".to_owned(),
                click: false,
                state: ItemState::Upcoming,
                is_next: false,
                loading: false,
                warning: Some(MISSING.to_owned()),
                playable: false,
                transition: chip(Transition::CueNext, "Cue", "Cue next"),
            },
            StripItem {
                index: 3,
                number: "4".to_owned(),
                title: "Click".to_owned(),
                meta: "70 BPM · 6/8 · 8 bars".to_owned(),
                click: true,
                state: ItemState::Upcoming,
                is_next: true,
                loading: true,
                warning: None,
                playable: true,
                transition: chip(Transition::Stay, "Stay", "Stay"),
            },
            StripItem {
                index: 4,
                number: "5".to_owned(),
                title: "Rivers Rise".to_owned(),
                meta: "128 BPM · Default".to_owned(),
                click: false,
                state: ItemState::Upcoming,
                is_next: false,
                loading: false,
                warning: Some("44100 Hz: plays once the device reopens at its rate".to_owned()),
                playable: true,
                transition: None,
            },
        ]
    );
}

#[test]
fn a_click_tile_without_bars_plays_until_the_operator_moves_on_and_a_buffered_next_is_not_loading()
{
    let mut info = info();
    info.items[3] = playable(
        SetlistItem::Click(ClickItem::new(
            120.0,
            TimeSignature {
                numerator: 4,
                denominator: 4,
            },
        )),
        None,
    );
    let mut snapshot = snapshot();
    snapshot.setlist.next_ready = true;

    let view = SetlistView::new(&info, &snapshot, &catalog(), None);

    assert_eq!(view.items[3].meta, "120 BPM · 4/4 · until you move on");
    assert!(view.items[3].is_next && !view.items[3].loading);
}

#[test]
fn the_problems_before_play_list_items_that_cannot_play_and_songs_at_another_rate() {
    let view = SetlistView::new(&info(), &snapshot(), &catalog(), None);

    assert_eq!(
        view.problems,
        [
            format!("Item 3 \"Missing\" cannot play: {MISSING}"),
            OTHER_RATE.to_owned(),
        ]
    );
}

#[test]
fn the_problems_during_play_show_the_setlist_problem_and_a_refused_control() {
    for (problem, line) in [
        (
            SetlistProblem::LateLink { item: 3 },
            "item 4 was not loaded in time to follow on; it started once it was",
        ),
        (
            SetlistProblem::Prepare {
                item: 4,
                error: "the song has no position 7".to_owned(),
            },
            "item 5 could not be prepared: the song has no position 7",
        ),
        (
            SetlistProblem::Transition {
                item: 3,
                error: SetlistCommandError::Runtime(RuntimeError::QueueFull {
                    command: "select".to_owned(),
                }),
            },
            "item 4 could not follow on: the audio thread has not taken the last commands yet; select was not sent",
        ),
    ] {
        let mut snapshot = snapshot();
        snapshot.setlist.problem = Some(problem);
        let view = SetlistView::new(&info(), &snapshot, &catalog(), None);
        assert_eq!(view.problems.last().map(String::as_str), Some(line));
    }

    let view = SetlistView::new(
        &info(),
        &snapshot(),
        &catalog(),
        Some(&SetlistCommandError::NoNext),
    );
    assert_eq!(
        view.problems.last().map(String::as_str),
        Some("Refused: no item after the current one can play")
    );
}

#[test]
fn the_problems_show_the_device_held_for_another_rate() {
    let mut snapshot = snapshot();
    snapshot.setlist.current = 4;
    snapshot.setlist.next = None;
    let error = OutputError::UnsupportedSampleRate {
        device: "Speakers".to_owned(),
        requested: 44_100,
        supported: vec![(48_000, 48_000)],
    };
    snapshot.song.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::StreamInvalidated,
        reopen_error: Some(error.clone()),
    };

    let view = SetlistView::new(&info(), &snapshot, &catalog(), None);

    assert!(!view.has_next);
    assert_eq!(
        view.problems.last(),
        Some(&format!(
            "The device is held for item 5 \"Rivers Rise\" at 44100 Hz, which it refuses ({error}); choose an item at 48000 Hz to play again"
        ))
    );
}

#[test]
fn a_crossfade_under_way_shows_both_items_and_its_progress() {
    let mut snapshot = snapshot();
    snapshot.setlist.blend = Some(BlendView {
        from: 0,
        to: 1,
        frames: 240_000,
        played: 96_000,
    });

    let view = SetlistView::new(&info(), &snapshot, &catalog(), None);

    assert_eq!(
        view.blend.as_deref(),
        Some("Crossfade 1 \"Open the Gates\" → 2 \"First Light\" · 40%")
    );
}

#[test]
fn the_first_playable_item_has_no_previous() {
    let mut snapshot = snapshot();
    snapshot.setlist.current = 0;
    snapshot.setlist.next = Some(1);
    let view = SetlistView::new(&info(), &snapshot, &catalog(), None);
    assert!(!view.has_previous && view.has_next);
    assert_eq!(view.position, "1 of 5");
}

fn song_info() -> SongInfo {
    SongInfo {
        title: "First Light".to_owned(),
        artist: None,
        sample_rate: RATE,
        arrangement: "long".to_owned(),
        positions: vec![PositionInfo {
            section_id: "verse".to_owned(),
            label: "Verse 1".to_owned(),
            start_frame: 0,
            length_frames: 96_000,
            start_bar: 1,
        }],
        warnings: Vec::new(),
    }
}

#[test]
fn previous_next_a_tile_and_back_to_start_map_to_setlist_commands() {
    let snapshot = snapshot();
    let song = song_info();
    let command = |action| setlist_command_for(action, &snapshot, &song);

    assert_eq!(
        command(UiAction::PreviousSong),
        Some(SetlistCommand::Previous)
    );
    assert_eq!(command(UiAction::NextSong), Some(SetlistCommand::Next));
    assert_eq!(
        command(UiAction::SelectItem(3)),
        Some(SetlistCommand::Select(3))
    );
    assert_eq!(
        command(UiAction::BackToStart),
        Some(SetlistCommand::Item(Command::BackToStart))
    );
    // Every other control acts on the current item, as for a single song.
    assert_eq!(
        command(UiAction::TogglePlay),
        Some(SetlistCommand::Item(Command::Pause))
    );
    assert_eq!(
        command(UiAction::QueueSection(0)),
        Some(SetlistCommand::Item(Command::QueueJump { position: 0 }))
    );
    assert_eq!(command(UiAction::QueueSection(1)), None);
}

#[test]
fn the_comma_and_period_keys_move_through_the_setlist() {
    let press = |key| KeyPress {
        key,
        mods: Mods::default(),
    };
    assert_eq!(
        key_down(&press(".")),
        Some(KeyAction::Ui(UiAction::NextSong))
    );
    assert_eq!(
        key_down(&press(",")),
        Some(KeyAction::Ui(UiAction::PreviousSong))
    );
    let held = KeyPress {
        key: ".",
        mods: Mods {
            alt: true,
            ..Mods::default()
        },
    };
    assert_eq!(key_down(&held), None);
}

#[test]
fn a_single_song_ignores_the_setlist_controls_and_keeps_its_own() {
    let snapshot = song_snapshot();
    let song = song_info();
    for action in [
        UiAction::PreviousSong,
        UiAction::NextSong,
        UiAction::SelectItem(0),
    ] {
        assert_eq!(command_for(action, &snapshot, &song), None, "{action:?}");
    }
    assert_eq!(
        command_for(UiAction::TogglePlay, &snapshot, &song),
        Some(Command::Pause)
    );
    assert_eq!(
        command_for(UiAction::BackToStart, &snapshot, &song),
        Some(Command::BackToStart)
    );
}

#[test]
fn a_setlist_file_names_its_library_by_the_setlists_folder_it_is_in() {
    // Absolute on every platform: on Windows `/music` is relative to the
    // current drive, and a setlist path is made absolute before use.
    let library = &std::path::absolute("/music/Library").unwrap();
    assert_eq!(
        Source::setlist_file(&library.join("Setlists/Sunday.setlist.json")),
        Some(Source::Setlist {
            library: library.to_owned(),
            file: "Sunday.setlist.json".to_owned(),
        })
    );
    assert_eq!(
        Source::setlist_file(&library.join("setlists/Sunday.setlist.json")),
        Some(Source::Setlist {
            library: library.to_owned(),
            file: "Sunday.setlist.json".to_owned(),
        })
    );
    assert_eq!(
        Source::setlist_file(&library.join("Other/Sunday.setlist.json")),
        None
    );
    assert_eq!(Source::setlist_file(Path::new("Sunday.setlist.json")), None);
}

#[test]
fn a_path_to_open_is_a_setlist_file_or_a_song_folder() {
    // Absolute on every platform: on Windows `/music` is relative to the
    // current drive, and a setlist path is made absolute before use.
    let library = &std::path::absolute("/music/Library").unwrap();
    assert_eq!(
        Source::for_path(&library.join("Setlists/Sunday.setlist.json")),
        Ok(Source::Setlist {
            library: library.to_owned(),
            file: "Sunday.setlist.json".to_owned(),
        })
    );
    assert_eq!(
        Source::for_path(&library.join("Songs/Example.song")),
        Ok(Source::Song(library.join("Songs/Example.song")))
    );
    assert_eq!(
        Source::for_path(Path::new("/tmp/Sunday.setlist.json")),
        Err(
            "/tmp/Sunday.setlist.json is not in the Setlists folder of a library folder, so its songs cannot be found"
                .to_owned()
        )
    );
}

#[test]
fn a_picked_setlist_must_be_a_setlist_file_in_a_setlists_folder() {
    // Absolute on every platform: on Windows `/music` is relative to the
    // current drive, and a setlist path is made absolute before use.
    let library = &std::path::absolute("/music/Library").unwrap();
    assert_eq!(
        Source::picked_setlist(&library.join("setlists/Sunday.setlist.json")),
        Ok(Source::Setlist {
            library: library.to_owned(),
            file: "Sunday.setlist.json".to_owned(),
        })
    );
    assert_eq!(
        Source::picked_setlist(Path::new("/music/notes.txt")),
        Err(
            "/music/notes.txt is not a setlist: pick a file ending in .setlist.json in the Setlists folder of a library folder"
                .to_owned()
        )
    );
    assert!(Source::picked_setlist(Path::new("/tmp/Sunday.setlist.json")).is_err());
}

#[test]
fn a_setlist_path_relative_to_the_current_folder_names_a_library_that_opens() {
    let Source::Setlist { library, file } =
        Source::for_path(Path::new("Setlists/Sunday.setlist.json")).unwrap()
    else {
        panic!("a setlist")
    };
    assert_eq!(file, "Sunday.setlist.json");
    assert_eq!(
        std::fs::canonicalize(&library).unwrap(),
        std::fs::canonicalize(".").unwrap()
    );
}

#[test]
fn dots_in_a_setlist_path_are_resolved_by_name() {
    // Absolute on every platform: on Windows `/music` is relative to the
    // current drive, and a setlist path is made absolute before use.
    let library = &std::path::absolute("/music/Library").unwrap();
    for path in [
        "/music/Library/./Setlists/Sunday.setlist.json",
        "/music/Library/Other/../Setlists/Sunday.setlist.json",
        "/music/Library/Setlists/../Setlists/./Sunday.setlist.json",
    ] {
        assert_eq!(
            Source::setlist_file(Path::new(path)),
            Some(Source::Setlist {
                library: library.to_owned(),
                file: "Sunday.setlist.json".to_owned(),
            }),
            "{path}"
        );
    }
    assert_eq!(
        Source::setlist_file(Path::new("/music/Library/Setlists/..")),
        None
    );
}

#[test]
fn clicking_the_current_tile_does_nothing() {
    let snapshot = snapshot();
    let song = song_info();
    assert_eq!(
        setlist_command_for(UiAction::SelectItem(1), &snapshot, &song),
        None,
        "item 2 is the current one"
    );
    assert_eq!(
        setlist_command_for(UiAction::SelectItem(0), &snapshot, &song),
        Some(SetlistCommand::Select(0))
    );
}

#[test]
fn a_link_into_a_song_at_another_rate_shows_as_the_cue_next_it_plays_as() {
    let mut info = info();
    // The click tile plays at the rate of the song before it (48 kHz);
    // Rivers Rise runs at 44.1 kHz, so the session reports the fallback.
    let SetlistItem::Click(click) = &mut info.items[3].item else {
        panic!("a click tile");
    };
    click.transition = Transition::AutoLink;
    info.items[3].fallback = Some(RateFallback {
        next: 4,
        next_rate: 44_100,
        device_rate: RATE,
    });

    let view = SetlistView::new(&info, &snapshot(), &catalog(), None);

    assert_eq!(
        view.items[3].transition,
        Some(TransitionChip {
            transition: Transition::CueNext,
            written: Transition::AutoLink,
            short: "Cue",
            label: "Transition: AutoLink, played as Cue next: item 5 runs at 44100 Hz and the device at 48000 Hz".to_owned(),
        })
    );
    assert!(
        view.problems.contains(
            &"Item 4 \"Click\" leads into item 5 \"Rivers Rise\" with Cue next, not AutoLink: item 5 runs at 44100 Hz and the device at 48000 Hz".to_owned()
        ),
        "{:?}",
        view.problems
    );
    // A link between items at one rate is as written.
    assert_eq!(
        view.items[0].transition.as_ref().unwrap().transition,
        Transition::AutoLink
    );
}

#[test]
fn the_current_items_chip_follows_the_session_at_the_rate_the_device_runs_now() {
    let mut snapshot = snapshot();
    // Item 2's Crossfade into the click tile, at the device's rate now.
    snapshot.setlist.fallback = Some(RateFallback {
        next: 3,
        next_rate: 44_100,
        device_rate: RATE,
    });

    let view = SetlistView::new(&info(), &snapshot, &catalog(), None);

    let chip = view.items[1].transition.clone().unwrap();
    assert_eq!(
        (chip.transition, chip.written),
        (Transition::CueNext, Transition::Crossfade)
    );
}
