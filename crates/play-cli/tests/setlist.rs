//! S4: the setlist mode's keys, its status line, and the problems printed
//! before a setlist plays.

use std::path::{Path, PathBuf};
use std::time::Duration;

use app::{
    BlendView, Command, DeviceStatus, FadeState, FadeView, ItemInfo, JumpTiming, MixerView,
    OutputError, PositionInfo, RateFallback, RuntimeError, SetlistCommand, SetlistCommandError,
    SetlistInfo, SetlistProblem, SetlistSnapshot, SetlistState, Snapshot, SongInfo, StreamInfo,
    TransportState,
};
use domain::{ClickItem, SetlistItem, SongItem, TimeSignature, Transition};
use play_cli::{
    KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, SetlistAction, SetlistKeys, action_for_key, item_label,
    setlist_location, setlist_problems, setlist_status_line, status_line,
};

fn song_item(path: &str, transition: Transition) -> SetlistItem {
    SetlistItem::Song(SongItem {
        transition,
        ..SongItem::new(path)
    })
}

fn click(bars: Option<u32>) -> SetlistItem {
    SetlistItem::Click(ClickItem {
        bars,
        ..ClickItem::new(
            120.0,
            TimeSignature {
                numerator: 6,
                denominator: 8,
            },
        )
    })
}

fn item(item: SetlistItem, sample_rate: Option<u32>, problems: &[&str]) -> ItemInfo {
    ItemInfo {
        item,
        sample_rate,
        problems: problems.iter().map(|p| (*p).to_owned()).collect(),
        fallback: None,
    }
}

/// Missing (unplayable), Opening (AutoLink), Hi at another rate, a click
/// tile, Closing.
fn info() -> SetlistInfo {
    SetlistInfo {
        name: "Sunday".to_owned(),
        sample_rate: 48_000,
        items: vec![
            item(
                song_item("Songs/Missing.song", Transition::CueNext),
                None,
                &["song \"Songs/Missing.song\" cannot be loaded: no such folder"],
            ),
            item(
                song_item("Songs/Opening.song", Transition::AutoLink),
                Some(48_000),
                &[],
            ),
            item(
                song_item("Hymns/Hi.song", Transition::Crossfade),
                Some(96_000),
                &[],
            ),
            item(click(Some(4)), None, &[]),
            item(
                song_item("Closing.song", Transition::Stay),
                Some(48_000),
                &[],
            ),
        ],
    }
}

fn song() -> SongInfo {
    SongInfo {
        title: "Opening".to_owned(),
        artist: None,
        sample_rate: 48_000,
        arrangement: "default".to_owned(),
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

fn snapshot() -> Snapshot {
    Snapshot {
        transport: TransportState::Playing,
        output_frame: 120_000,
        source_frame: 120_000,
        bar: 3,
        beat: 2,
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
            sample_rate: 48_000,
            buffer_frames: 256,
            channels: 2,
        }),
        mixer: MixerView::default(),
        underruns: 0,
        decode_errors: 0,
        last_error: None,
    }
}

/// Item 2 (index 1, Opening) playing, AutoLink into Hi, which is ready.
fn setlist_snapshot() -> SetlistSnapshot {
    SetlistSnapshot {
        song: snapshot(),
        setlist: SetlistState {
            current: 1,
            next: Some(2),
            transition: Transition::AutoLink,
            fallback: None,
            next_ready: true,
            blend: None,
            problem: None,
        },
    }
}

fn send(command: SetlistCommand) -> Option<SetlistAction> {
    Some(SetlistAction::Send(command))
}

#[test]
fn comma_and_period_select_the_previous_and_next_item() {
    let mut keys = SetlistKeys::default();
    let now = setlist_snapshot();
    assert_eq!(keys.key('.', &now), send(SetlistCommand::Next));
    assert_eq!(keys.key(',', &now), send(SetlistCommand::Previous));
}

#[test]
fn g_then_a_number_and_enter_selects_that_item() {
    let mut keys = SetlistKeys::default();
    let now = setlist_snapshot();
    assert_eq!(keys.key('g', &now), None);
    assert_eq!(keys.entry(), Some(""));
    assert_eq!(
        keys.key('1', &now),
        None,
        "a digit types, it queues no section"
    );
    assert_eq!(keys.key('2', &now), None);
    assert_eq!(keys.entry(), Some("12"));
    assert_eq!(keys.key(KEY_BACKSPACE, &now), None);
    assert_eq!(keys.entry(), Some("1"));
    assert_eq!(keys.key('0', &now), None);
    assert_eq!(keys.key(KEY_ENTER, &now), send(SetlistCommand::Select(9)));
    assert_eq!(keys.entry(), None, "Enter ends it");

    assert_eq!(keys.key('G', &now), None, "either case");
    assert_eq!(keys.key('3', &now), None);
    assert_eq!(keys.key(KEY_ESCAPE, &now), None);
    assert_eq!(keys.entry(), None, "Escape cancels");
    assert_eq!(
        keys.key('3', &now),
        send(SetlistCommand::Item(Command::QueueJump { position: 2 })),
        "after it, digits queue sections again"
    );

    for nothing in ["", "0", "00"] {
        keys.key('g', &now);
        for digit in nothing.chars() {
            keys.key(digit, &now);
        }
        assert_eq!(keys.key(KEY_ENTER, &now), None, "{nothing:?} names no item");
        assert_eq!(keys.entry(), None);
    }

    keys.key('g', &now);
    for _ in 0..9 {
        keys.key('9', &now);
    }
    assert_eq!(keys.entry(), Some("9999"), "four digits at most");
}

#[test]
fn another_key_cancels_the_item_number_and_does_its_own_job() {
    let mut keys = SetlistKeys::default();
    let now = setlist_snapshot();
    keys.key('g', &now);
    keys.key('2', &now);
    assert_eq!(
        keys.key(' ', &now),
        send(SetlistCommand::Item(Command::Pause))
    );
    assert_eq!(keys.entry(), None);
}

#[test]
fn every_song_key_keeps_its_meaning_for_the_current_item() {
    let mut keys = SetlistKeys::default();
    let now = setlist_snapshot();
    for key in ['s', 'b', '1', '9', 'n', 'l', 'i', 'f', 'c', 'x', ' ', 'S'] {
        let Some(play_cli::Action::Send(command)) = action_for_key(key, &now.song) else {
            panic!("{key:?} is a song key");
        };
        assert_eq!(
            keys.key(key, &now),
            send(SetlistCommand::Item(command)),
            "{key:?}"
        );
    }
    assert_eq!(keys.key('q', &now), Some(SetlistAction::Quit));
    for key in ['0', 'a', 'z', KEY_ENTER, KEY_ESCAPE] {
        assert_eq!(keys.key(key, &now), None, "{key:?}");
    }
}

#[test]
fn single_song_keys_are_unchanged_and_ignore_the_setlist_keys() {
    let now = snapshot();
    for key in [',', '.', 'g', 'G'] {
        assert_eq!(action_for_key(key, &now), None, "{key:?}");
    }
    assert_eq!(
        action_for_key('1', &now),
        Some(play_cli::Action::Send(Command::QueueJump { position: 0 }))
    );
}

#[test]
fn items_are_named_by_song_folder_or_click_tempo_and_meter() {
    assert_eq!(
        item_label(&song_item("Hymns/Hi.song", Transition::CueNext)),
        "Hi"
    );
    assert_eq!(
        item_label(&song_item("Closing.song", Transition::CueNext)),
        "Closing"
    );
    assert_eq!(
        item_label(&song_item("Old\\Folder", Transition::CueNext)),
        "Folder"
    );
    assert_eq!(item_label(&click(Some(4))), "Click 120 BPM 6/8, 4 bars");
    assert_eq!(item_label(&click(Some(1))), "Click 120 BPM 6/8, 1 bar");
    assert_eq!(item_label(&click(None)), "Click 120 BPM 6/8");
}

#[test]
fn problems_name_unplayable_and_other_rate_items_before_anything_plays() {
    assert_eq!(
        setlist_problems(&info()),
        [
            "item 1 (\"Missing\") cannot play: song \"Songs/Missing.song\" cannot be loaded: no such folder",
            "item 3 (\"Hi\") runs at 96000 Hz and the device at 48000 Hz: it plays once the device reopens at its rate, and no AutoLink or Crossfade leads into it",
        ]
    );
}

#[test]
fn the_setlist_status_shows_the_item_what_follows_and_then_the_song() {
    let keys = SetlistKeys::default();
    let line = setlist_status_line(&setlist_snapshot(), &info(), &song(), &keys, None);
    let expected = format!(
        "item 2/5 \"Opening\" | then: AutoLink -> 3 \"Hi\" | {}",
        status_line(&snapshot(), &song())
    );
    assert_eq!(line, expected);
}

#[test]
fn the_setlist_status_shows_loading_stay_the_end_a_blend_an_entry_and_problems() {
    let info = info();
    let song = song();
    let mut keys = SetlistKeys::default();
    let line = |snapshot: &SetlistSnapshot, keys: &SetlistKeys, refused| {
        setlist_status_line(snapshot, &info, &song, keys, refused)
    };
    let first_fields = |line: String| -> Vec<String> {
        line.split(" | ")
            .filter(|field| {
                field.starts_with("item ")
                    || field.starts_with("then:")
                    || field.starts_with("crossfade")
                    || field.starts_with("go to")
                    || field.starts_with("! ")
            })
            .map(str::to_owned)
            .collect()
    };

    let loading = SetlistSnapshot {
        setlist: SetlistState {
            next_ready: false,
            transition: Transition::CueNext,
            ..setlist_snapshot().setlist
        },
        ..setlist_snapshot()
    };
    assert_eq!(
        first_fields(line(&loading, &keys, None)),
        [
            "item 2/5 \"Opening\"",
            "then: Cue next -> 3 \"Hi\" (loading)"
        ]
    );

    let stay = SetlistSnapshot {
        setlist: SetlistState {
            current: 4,
            next: None,
            transition: Transition::Stay,
            ..setlist_snapshot().setlist
        },
        ..setlist_snapshot()
    };
    assert_eq!(
        first_fields(line(&stay, &keys, None)),
        ["item 5/5 \"Closing\"", "then: Stay"]
    );
    let last = SetlistSnapshot {
        setlist: SetlistState {
            transition: Transition::AutoLink,
            ..stay.setlist.clone()
        },
        ..stay.clone()
    };
    assert_eq!(
        first_fields(line(&last, &keys, None)),
        ["item 5/5 \"Closing\"", "then: end of setlist"]
    );

    let blending = SetlistSnapshot {
        setlist: SetlistState {
            current: 2,
            next: Some(3),
            transition: Transition::Crossfade,
            blend: Some(BlendView {
                from: 1,
                to: 2,
                frames: 240_000,
                played: 96_000,
            }),
            problem: Some(SetlistProblem::LateLink { item: 2 }),
            ..setlist_snapshot().setlist
        },
        ..setlist_snapshot()
    };
    let refused = SetlistCommandError::NoNext;
    keys.key('g', &blending);
    keys.key('4', &blending);
    let text = line(&blending, &keys, Some(&refused));
    assert_eq!(
        first_fields(text.clone()),
        [
            "go to item 4_ (Enter, Esc)",
            "item 3/5 \"Hi\"",
            "crossfade from 2 \"Opening\" 40%",
            "then: Crossfade -> 4 \"Click 120 BPM 6/8, 4 bars\"",
            "! item 3 was not loaded in time to follow on; it started once it was",
            "! no item after the current one can play",
        ]
    );
    assert!(
        text.ends_with("! no item after the current one can play"),
        "{text}"
    );

    let held = SetlistSnapshot {
        song: Snapshot {
            device: DeviceStatus::Disconnected {
                device: "Speakers".to_owned(),
                reason: app::LossReason::StreamInvalidated,
                reopen_error: Some(OutputError::UnsupportedSampleRate {
                    device: "Speakers".to_owned(),
                    requested: 96_000,
                    supported: vec![(48_000, 48_000)],
                }),
            },
            last_error: Some(RuntimeError::QueueFull {
                command: "Select".to_owned(),
            }),
            ..snapshot()
        },
        setlist: SetlistState {
            problem: Some(SetlistProblem::Transition {
                item: 2,
                error: SetlistCommandError::Runtime(RuntimeError::QueueFull {
                    command: "SelectCued".to_owned(),
                }),
            }),
            ..setlist_snapshot().setlist
        },
    };
    let text = line(&held, &SetlistKeys::default(), None);
    assert!(
        text.contains("NO DEVICE: \"Speakers\" (cannot open it again: "),
        "{text}"
    );
    assert!(text.contains("cannot play at 96000 Hz"), "{text}");
    assert!(
        text.ends_with(
            "! item 3 could not follow on: the audio thread has not taken the last commands yet; SelectCued was not sent"
        ),
        "{text}"
    );
}

#[test]
fn a_setlist_file_is_found_in_the_setlists_folder_of_its_library() {
    let library = Path::new("music").join("Library");
    let file = library.join("Setlists").join("Sunday Morning.setlist.json");
    let (root, name) = setlist_location(&file).unwrap();
    assert!(root.ends_with(&library), "{}", root.display());
    assert!(root.is_absolute());
    assert_eq!(name, "Sunday Morning.setlist.json");

    let lower = library.join("setlists").join("Sunday.setlist.json");
    assert!(
        setlist_location(&lower).is_ok(),
        "the folder name in any case"
    );

    let elsewhere = PathBuf::from("Sunday.setlist.json");
    let error = setlist_location(&elsewhere).unwrap_err();
    assert!(error.contains("is not in a Setlists folder"), "{error}");
}

#[test]
fn delete_and_line_feed_act_as_backspace_and_enter() {
    let mut keys = SetlistKeys::default();
    let now = setlist_snapshot();
    keys.key('g', &now);
    keys.key('4', &now);
    keys.key('2', &now);
    assert_eq!(keys.key('\u{7f}', &now), None, "DEL deletes a digit");
    assert_eq!(keys.entry(), Some("4"));
    assert_eq!(keys.key('\n', &now), send(SetlistCommand::Select(3)));
}

#[test]
fn dot_dot_in_a_setlist_path_is_resolved_by_name() {
    let library = Path::new("music").join("Library");
    for path in [
        library
            .join("Setlists")
            .join("old")
            .join("..")
            .join("Sunday.setlist.json"),
        library
            .join("Setlists")
            .join("..")
            .join("Setlists")
            .join("Sunday.setlist.json"),
        library
            .join("x")
            .join("..")
            .join("Setlists")
            .join(".")
            .join("Sunday.setlist.json"),
    ] {
        let (root, name) = setlist_location(&path).unwrap();
        assert!(
            root.ends_with(&library),
            "{} -> {}",
            path.display(),
            root.display()
        );
        assert!(
            root.components().all(|part| !matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )),
            "{} -> {}",
            path.display(),
            root.display()
        );
        assert_eq!(name, "Sunday.setlist.json");
    }
    let parent_of_cwd = std::env::current_dir()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let (root, _) =
        setlist_location(&Path::new("..").join("Setlists").join("Sunday.setlist.json")).unwrap();
    assert_eq!(
        root, parent_of_cwd,
        "a leading .. counts from the current folder"
    );

    let up = library
        .join("Setlists")
        .join("Sunday.setlist.json")
        .join("..");
    assert!(
        setlist_location(&up).is_err(),
        "a folder is not a setlist file"
    );
}

#[test]
fn a_link_into_another_rate_shows_as_the_cue_next_it_plays_as() {
    let keys = SetlistKeys::default();
    let mut snapshot = setlist_snapshot();
    // Opening runs the device at 48 kHz; Hi runs at 96 kHz.
    snapshot.setlist.fallback = Some(RateFallback {
        next: 2,
        next_rate: 96_000,
        device_rate: 48_000,
    });

    let line = setlist_status_line(&snapshot, &info(), &song(), &keys, None);

    let expected = format!(
        "item 2/5 \"Opening\" | then: Cue next (AutoLink into 96000 Hz) -> 3 \"Hi\" | {}",
        status_line(&snapshot.song, &song())
    );
    assert_eq!(line, expected);
}
