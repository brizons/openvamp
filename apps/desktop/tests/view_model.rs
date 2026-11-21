//! S6: the Perform screen's view-model, without a window. One test per
//! element: song map, queued-jump marker, Now/Next, pads, transport, fade,
//! hold-to-panic, jump timing, loop and infinite, the status area, and the
//! keys.

use std::time::{Duration, Instant};

use app::{
    Command, DeviceStatus, FadeState, FadeView, JumpKind, JumpTiming, LoadError, LossReason,
    MixerView, OpenError, OutputError, PositionInfo, QueuedView, RuntimeError, Snapshot, SongInfo,
    SongProblem, StreamInfo, TransportState,
};
use openvamp_desktop::action::{UiAction, command_for};
use openvamp_desktop::hold::{PANIC_HOLD, PanicHold};
use openvamp_desktop::keys::{
    HeldKeys, KeyAction, KeyPlatform, KeyPress, Mods, PANIC_KEY, SECONDARY_IS_PLATFORM,
    is_panic_key, key_down,
};
use openvamp_desktop::view::{
    Level, PadGrid, PerformView, SectionKind, SongFacts, open_error_lines, pad_grid, section_icon,
    short_label,
};

const RATE: u32 = 48_000;
const SECTION: u64 = 96_000;

/// Intro, Verse 1, Chorus (2 s each) and Outro (1 s): 7 s in all.
fn song() -> SongInfo {
    let position = |id: &str, label: &str, start_frame, length_frames, start_bar| PositionInfo {
        section_id: id.to_owned(),
        label: label.to_owned(),
        start_frame,
        length_frames,
        start_bar,
    };
    SongInfo {
        title: "Example".to_owned(),
        artist: Some("The Band".to_owned()),
        sample_rate: RATE,
        arrangement: "default".to_owned(),
        positions: vec![
            position("intro", "Intro", 0, SECTION, 1),
            position("verse", "Verse 1", SECTION, SECTION, 2),
            position("chorus", "Chorus", 2 * SECTION, SECTION, 3),
            position("outro", "Outro", 3 * SECTION, SECTION / 2, 4),
        ],
        warnings: Vec::new(),
    }
}

/// Playing a quarter into Verse 1 (position 1), nothing queued.
fn snapshot() -> Snapshot {
    Snapshot {
        transport: TransportState::Playing,
        output_frame: 120_000,
        source_frame: SECTION + 24_000,
        bar: 3,
        beat: 2,
        position: 1,
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

fn queued(target: usize, kind: JumpKind, frames_until: u64) -> QueuedView {
    QueuedView {
        target,
        target_label: song().positions[target].label.clone(),
        kind,
        executes_at: 120_000 + frames_until,
        frames_until,
        deferrals: 0,
    }
}

fn close(actual: f32, expected: f64) {
    assert!(
        (f64::from(actual) - expected).abs() < 1e-6,
        "{actual} is not {expected}"
    );
}

#[test]
fn song_map_lists_the_sections_in_arrangement_order_sized_by_length() {
    let view = PerformView::new(&snapshot(), &song());
    let blocks = &view.map.blocks;

    let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["Intro", "Verse 1", "Chorus", "Outro"]);
    let shorts: Vec<&str> = blocks.iter().map(|b| b.short.as_str()).collect();
    assert_eq!(shorts, ["In", "V1", "C", "Out"]);
    let kinds: Vec<SectionKind> = blocks.iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        [
            SectionKind::Neutral,
            SectionKind::Verse,
            SectionKind::Chorus,
            SectionKind::Neutral
        ]
    );
    // 2 s, 2 s, 2 s and 1 s of 7 s.
    for (block, (start, width)) in
        blocks
            .iter()
            .zip([(0.0, 2.0), (2.0, 2.0), (4.0, 2.0), (6.0, 1.0)])
    {
        close(block.start, start / 7.0);
        close(block.width, width / 7.0);
    }
    let now: Vec<bool> = blocks.iter().map(|b| b.is_now).collect();
    assert_eq!(now, [false, true, false, false]);
    assert!(blocks.iter().all(|b| !b.is_queued));
    assert_eq!(view.map.jump, None);
}

#[test]
fn song_map_playhead_follows_the_position_in_the_arrangement() {
    // 2 s of intro plus 0.5 s into the verse, of 7 s.
    let view = PerformView::new(&snapshot(), &song());
    close(view.map.playhead, 2.5 / 7.0);
    assert_eq!(view.clock, "00:02 / 00:07");
    assert_eq!(view.bar_beat, "Bar 3 · beat 2");

    // A repeated or jumped-to section places the playhead by position, not
    // by source frame: the chorus played as position 2.
    let mut snapshot = snapshot();
    snapshot.position = 2;
    snapshot.source_frame = 2 * SECTION + SECTION / 2;
    close(PerformView::new(&snapshot, &song()).map.playhead, 5.0 / 7.0);
}

#[test]
fn queued_jump_marks_its_target_and_where_and_when_it_executes() {
    let mut snapshot = snapshot();
    snapshot.queued = Some(queued(3, JumpKind::Jump, 24_000));
    let view = PerformView::new(&snapshot, &song());

    let targets: Vec<bool> = view.map.blocks.iter().map(|b| b.is_queued).collect();
    assert_eq!(targets, [false, false, false, true]);
    // The playhead at 2.5 s plus 0.5 s until it executes.
    let marker = view.map.jump.expect("a queued jump shows on the map");
    close(marker.at, 3.0 / 7.0);
    assert_eq!(marker.label, "Jump · in 0.5 s");
    assert_eq!(view.next.heading, "Jump queued");
    assert_eq!(view.next.name, "Outro");
    assert_eq!(view.next.detail, "Executes in 0.5 s");
    assert!(view.next.queued);

    // It never shows past the end of the section it leaves.
    snapshot.queued = Some(queued(3, JumpKind::Jump, 10 * SECTION));
    let marker = PerformView::new(&snapshot, &song()).map.jump.unwrap();
    close(marker.at, 4.0 / 7.0);

    // A jump waiting for its audio says how long it has waited.
    let mut waiting = queued(3, JumpKind::Jump, 24_000);
    waiting.deferrals = 2;
    snapshot.queued = Some(waiting);
    let view = PerformView::new(&snapshot, &song());
    assert_eq!(
        view.next.detail,
        "Executes in 0.5 s · moved on 2 boundaries waiting for its audio"
    );

    // A live loop is marked the same way, on the current section.
    snapshot.queued = Some(queued(1, JumpKind::Loop, 72_000));
    let view = PerformView::new(&snapshot, &song());
    assert_eq!(view.map.jump.unwrap().label, "Loop · in 1.5 s");
    assert_eq!(view.next.heading, "Loop queued");
    assert_eq!(view.next.name, "Verse 1");
}

#[test]
fn pads_mark_now_from_the_position_and_next_from_what_plays_after_it() {
    let marks = |view: &PerformView| -> Vec<(bool, bool, bool)> {
        view.pads
            .iter()
            .map(|p| (p.is_now, p.is_next, p.is_queued))
            .collect()
    };
    let song = song();

    // Nothing queued: the next position plays next.
    let view = PerformView::new(&snapshot(), &song);
    let labels: Vec<&str> = view.pads.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["Intro", "Verse 1", "Chorus", "Outro"]);
    let keys: Vec<Option<char>> = view.pads.iter().map(|p| p.key).collect();
    assert_eq!(keys, [Some('1'), Some('2'), Some('3'), Some('4')]);
    assert_eq!(
        marks(&view),
        [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, false)
        ]
    );
    assert_eq!(view.now.name, "Verse 1");
    assert_eq!(view.now.detail, "1.5 s left");
    assert_eq!(view.next.heading, "Next");
    assert_eq!(view.next.name, "Chorus");
    assert_eq!(view.next.detail, "Plays in 1.5 s · tap a section to jump");
    assert!(!view.next.queued);

    // A queued jump plays next instead.
    let mut snapshot = snapshot();
    snapshot.queued = Some(queued(0, JumpKind::Jump, 72_000));
    assert_eq!(
        marks(&PerformView::new(&snapshot, &song)),
        [
            (false, true, true),
            (true, false, false),
            (false, false, false),
            (false, false, false)
        ]
    );

    // The infinite loop repeats the current section.
    snapshot.queued = None;
    snapshot.infinite = true;
    let view = PerformView::new(&snapshot, &song);
    assert_eq!(view.pads[1].is_now, view.pads[1].is_next);
    assert!(view.pads[1].is_next && !view.pads[1].is_queued);
    assert_eq!(view.next.name, "Verse 1");
    assert_eq!(view.next.detail, "Repeats in 1.5 s · infinite loop");

    // After the last section, the song ends.
    snapshot.infinite = false;
    snapshot.position = 3;
    snapshot.section = "Outro".to_owned();
    snapshot.source_frame = 3 * SECTION;
    let view = PerformView::new(&snapshot, &song);
    assert!(view.pads.iter().all(|p| !p.is_next));
    assert_eq!(view.next.name, "End of song");
    assert_eq!(view.next.detail, "Ends in 1.0 s");
}

#[test]
fn a_pad_queues_a_jump_and_the_queued_pad_cancels_it() {
    let song = song();
    let mut snapshot = snapshot();
    assert_eq!(
        command_for(UiAction::QueueSection(2), &snapshot, &song),
        Some(Command::QueueJump { position: 2 })
    );
    snapshot.queued = Some(queued(2, JumpKind::Jump, 72_000));
    assert_eq!(
        command_for(UiAction::QueueSection(2), &snapshot, &song),
        Some(Command::CancelJump)
    );
    assert_eq!(
        command_for(UiAction::QueueSection(0), &snapshot, &song),
        Some(Command::QueueJump { position: 0 })
    );
    // No pad, no command: a key past the arrangement does nothing.
    assert_eq!(
        command_for(UiAction::QueueSection(4), &snapshot, &song),
        None
    );
    assert_eq!(
        command_for(UiAction::CancelJump, &snapshot, &song),
        Some(Command::CancelJump)
    );
}

#[test]
fn a_hold_whose_press_is_lost_never_fires() {
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);
    let mut hold = PanicHold::new();

    // Halfway through, the window loses focus: the hold ends at once.
    hold.press(at(0));
    close(hold.fill(at(500)), 0.5);
    hold.cancel();
    assert!(!hold.is_counting());
    assert_eq!(hold.fill(at(500)), 0.0);
    assert!(
        !hold.tick(at(0) + PANIC_HOLD),
        "the full time passes, no panic"
    );
    assert!(
        !hold.release(at(2_000)),
        "a release that still arrives fires nothing"
    );

    // Cancelled with the full hold already reached but no tick yet: still
    // no panic.
    hold.press(at(3_000));
    hold.cancel();
    assert!(!hold.tick(at(5_000)));
    assert!(!hold.release(at(5_000)));

    // The next press is a fresh hold that fires.
    hold.press(at(6_000));
    assert!(hold.is_counting());
    assert!(hold.tick(at(6_000) + PANIC_HOLD));
}

#[test]
fn transport_plays_pauses_stops_and_goes_back_to_start() {
    let song = song();
    let mut snapshot = snapshot();
    let expected = [
        (TransportState::Stopped, Command::Play, "Stopped", "Play"),
        (
            TransportState::Starting,
            Command::Pause,
            "Loading…",
            "Pause",
        ),
        (TransportState::Playing, Command::Pause, "Playing", "Pause"),
        (TransportState::Pausing, Command::Play, "Paused", "Play"),
        (TransportState::Paused, Command::Play, "Paused", "Play"),
        (TransportState::Stopping, Command::Play, "Stopping", "Play"),
        (TransportState::Panicking, Command::Play, "Panic", "Play"),
    ];
    for (transport, command, state, label) in expected {
        snapshot.transport = transport;
        assert_eq!(
            command_for(UiAction::TogglePlay, &snapshot, &song),
            Some(command),
            "{transport:?}"
        );
        let view = PerformView::new(&snapshot, &song);
        assert_eq!(view.transport.state, state);
        assert_eq!(view.transport.play_label, label);
        assert_eq!(view.transport.playing, label == "Pause");
    }
    assert_eq!(
        command_for(UiAction::Stop, &snapshot, &song),
        Some(Command::Stop)
    );
    assert_eq!(
        command_for(UiAction::BackToStart, &snapshot, &song),
        Some(Command::BackToStart)
    );
}

#[test]
fn fade_toggles_and_shows_its_direction_and_progress() {
    let song = song();
    let mut snapshot = snapshot();
    assert_eq!(
        command_for(UiAction::ToggleFade, &snapshot, &song),
        Some(Command::ToggleFade)
    );
    let expected = [
        (FadeState::In, 1.0, "Fade", false),
        (FadeState::FadingOut, 0.6, "Fading out 60%", true),
        (FadeState::Out, 0.0, "Fade in", true),
        (FadeState::FadingIn, 0.25, "Fading in 25%", true),
    ];
    for (state, level, label, active) in expected {
        snapshot.fade.state = state;
        snapshot.fade.level = level;
        let fade = PerformView::new(&snapshot, &song).fade;
        assert_eq!((fade.label.as_str(), fade.active), (label, active));
    }
}

#[test]
fn hold_to_panic_needs_the_full_hold() {
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);

    // A click: down and up again at once.
    let mut hold = PanicHold::new();
    hold.press(at(0));
    assert!(!hold.tick(at(50)));
    assert!(!hold.release(at(80)));
    assert!(!hold.tick(at(2_000)), "a released hold never fires");
    assert_eq!(hold.fill(at(2_000)), 0.0);

    // Half a hold, then a fresh press starts from zero.
    hold.press(at(3_000));
    close(hold.fill(at(3_500)), 0.5);
    assert!(!hold.release(at(3_500)));
    hold.press(at(4_000));
    assert!(!hold.tick(at(4_900)));
    assert!(hold.is_counting());

    // The full hold fires once, on the first tick past it; while the
    // control stays down, nothing counts any more and the fill is empty.
    assert!(hold.tick(at(4_000) + PANIC_HOLD));
    assert!(!hold.is_counting());
    assert_eq!(hold.fill(at(5_100)), 0.0);
    assert!(!hold.tick(at(5_100)));
    assert!(!hold.release(at(5_200)));
    assert!(!hold.is_counting());

    // A key that repeats while held does not restart the hold.
    hold.press(at(6_000));
    hold.press(at(6_500));
    close(hold.fill(at(6_500)), 0.5);
    // Released after the full hold with no tick in between: it fires then.
    assert!(hold.release(at(7_100)));

    // The panic it fires is the app's panic.
    assert_eq!(
        command_for(UiAction::Panic, &snapshot(), &song()),
        Some(Command::Panic)
    );
}

#[test]
fn jump_timing_toggle_sends_the_timing_chosen() {
    let song = song();
    let mut snapshot = snapshot();
    assert_eq!(
        PerformView::new(&snapshot, &song).jump_timing,
        JumpTiming::SectionEnd
    );
    assert_eq!(
        command_for(
            UiAction::SetJumpTiming(JumpTiming::NextBar),
            &snapshot,
            &song
        ),
        Some(Command::SetJumpTiming(JumpTiming::NextBar))
    );
    assert_eq!(
        command_for(UiAction::ToggleJumpTiming, &snapshot, &song),
        Some(Command::SetJumpTiming(JumpTiming::NextBar))
    );
    snapshot.jump_timing = JumpTiming::NextBar;
    assert_eq!(
        PerformView::new(&snapshot, &song).jump_timing,
        JumpTiming::NextBar
    );
    assert_eq!(
        command_for(UiAction::ToggleJumpTiming, &snapshot, &song),
        Some(Command::SetJumpTiming(JumpTiming::SectionEnd))
    );
}

#[test]
fn loop_section_and_infinite_toggle_from_their_state() {
    let song = song();
    let mut snapshot = snapshot();
    let view = PerformView::new(&snapshot, &song);
    assert!(!view.live_loop && !view.infinite);
    assert_eq!(
        command_for(UiAction::ToggleLiveLoop, &snapshot, &song),
        Some(Command::LiveLoop)
    );
    assert_eq!(
        command_for(UiAction::ToggleInfinite, &snapshot, &song),
        Some(Command::SetInfinite(true))
    );

    snapshot.queued = Some(queued(1, JumpKind::Loop, 72_000));
    snapshot.infinite = true;
    let view = PerformView::new(&snapshot, &song);
    assert!(view.live_loop && view.infinite);
    assert_eq!(
        command_for(UiAction::ToggleLiveLoop, &snapshot, &song),
        Some(Command::CancelJump)
    );
    assert_eq!(
        command_for(UiAction::ToggleInfinite, &snapshot, &song),
        Some(Command::SetInfinite(false))
    );

    // A queued jump is not a loop: "Loop section" queues one over it.
    snapshot.queued = Some(queued(3, JumpKind::Jump, 72_000));
    assert!(!PerformView::new(&snapshot, &song).live_loop);
    assert_eq!(
        command_for(UiAction::ToggleLiveLoop, &snapshot, &song),
        Some(Command::LiveLoop)
    );
}

#[test]
fn status_area_shows_the_device_underruns_and_the_last_error_in_the_apps_words() {
    let mut song = song();
    song.warnings = vec!["stem \"pad\" is silent".to_owned()];
    let mut snapshot = snapshot();
    let status = PerformView::new(&snapshot, &song).status;
    assert_eq!(status.device, "Speakers · 48000 Hz · 256 frames · 5.3 ms");
    assert_eq!(status.device_level, Level::Ok);
    assert_eq!(status.underruns, "Underruns: 0");
    assert_eq!(status.last_error, None);
    assert_eq!(status.warnings, ["stem \"pad\" is silent"]);

    let error = RuntimeError::JumpWaiting {
        section: "Chorus".to_owned(),
        deferrals: 2,
    };
    snapshot.last_error = Some(error.clone());
    snapshot.underruns = 3;
    snapshot.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::Unplugged,
        reopen_error: None,
    };
    let status = PerformView::new(&snapshot, &song).status;
    assert_eq!(
        status.last_error.as_deref(),
        Some(
            "the jump to Chorus is waiting for its audio from disk; it has moved 2 boundary(ies) on"
        )
    );
    assert_eq!(status.last_error, Some(error.to_string()));
    assert_eq!(status.underruns, "Underruns: 3");
    assert_eq!(
        status.device,
        "No device: \"Speakers\" (the device was unplugged)"
    );
    assert_eq!(status.device_level, Level::Error);

    let refused = OutputError::NoDevice;
    snapshot.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::Unplugged,
        reopen_error: Some(refused.clone()),
    };
    assert_eq!(
        PerformView::new(&snapshot, &song).status.device,
        format!("No device: \"Speakers\" (cannot open it again: {refused})")
    );
    // The session closed the stream itself for new settings the device
    // refuses: the device is there.
    let refused = OutputError::UnsupportedBufferSize {
        device: "Speakers".to_owned(),
        requested: 1_024,
        min: 64,
        max: 512,
    };
    snapshot.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::Reconfigured,
        reopen_error: Some(refused.clone()),
    };
    assert_eq!(
        PerformView::new(&snapshot, &song).status.device,
        format!("Paused: \"Speakers\" refuses the new settings ({refused})")
    );
    snapshot.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::Reconfigured,
        reopen_error: None,
    };
    assert_eq!(
        PerformView::new(&snapshot, &song).status.device,
        "No device: \"Speakers\" (the stream closed to open again with new settings)"
    );
    snapshot.device = DeviceStatus::PlayerLost {
        device: "Speakers".to_owned(),
    };
    assert_eq!(
        PerformView::new(&snapshot, &song).status.device,
        "No device: \"Speakers\" (the output kept the player; open the song again)"
    );
}

#[test]
fn a_song_that_does_not_open_lists_every_problem() {
    let invalid = OpenError::Load(LoadError::Invalid(vec![
        SongProblem::StemOutsideFolder {
            id: "drums".to_owned(),
            file: "../drums.wav".to_owned(),
        },
        SongProblem::StemOutsideFolder {
            id: "keys".to_owned(),
            file: "/keys.wav".to_owned(),
        },
    ]));
    assert_eq!(
        open_error_lines(&invalid),
        [
            "stem \"drums\" file \"../drums.wav\" resolves outside the song folder",
            "stem \"keys\" file \"/keys.wav\" resolves outside the song folder",
        ]
    );
    let device = OpenError::Output(OutputError::NoDevice);
    assert_eq!(open_error_lines(&device), [device.to_string()]);
}

#[test]
fn keys_match_play_cli() {
    let plain = |key| {
        key_down(&KeyPress {
            key,
            mods: Mods::default(),
        })
    };
    let ui = |action| Some(KeyAction::Ui(action));
    assert_eq!(plain("space"), ui(UiAction::TogglePlay));
    assert_eq!(plain("s"), ui(UiAction::Stop));
    assert_eq!(plain("S"), ui(UiAction::Stop));
    assert_eq!(plain("b"), ui(UiAction::BackToStart));
    assert_eq!(plain("1"), ui(UiAction::QueueSection(0)));
    assert_eq!(plain("9"), ui(UiAction::QueueSection(8)));
    assert_eq!(plain("0"), None);
    assert_eq!(plain("n"), ui(UiAction::ToggleJumpTiming));
    assert_eq!(plain("l"), ui(UiAction::ToggleLiveLoop));
    assert_eq!(plain("i"), ui(UiAction::ToggleInfinite));
    assert_eq!(plain("f"), ui(UiAction::ToggleFade));
    assert_eq!(plain("c"), ui(UiAction::CancelJump));
    assert_eq!(plain("escape"), ui(UiAction::CancelJump));
    // Panic needs the key held, like the button.
    assert_eq!(plain("x"), Some(KeyAction::PanicDown));
    // Quitting needs the secondary modifier: a stray Q on stage does nothing.
    assert_eq!(plain("q"), None);

    let secondary = Mods {
        platform: SECONDARY_IS_PLATFORM,
        control: !SECONDARY_IS_PLATFORM,
        ..Mods::default()
    };
    let with = |key, mods| key_down(&KeyPress { key, mods });
    assert_eq!(with("q", secondary), Some(KeyAction::Quit));
    assert_eq!(with("o", secondary), Some(KeyAction::OpenSong));
    assert_eq!(with("x", secondary), None);
    assert_eq!(with("space", secondary), None);
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    assert_eq!(with("x", alt), None);
    assert_eq!(
        with(
            "q",
            Mods {
                alt: true,
                ..secondary
            }
        ),
        None
    );
}

#[test]
fn the_panic_key_is_one_key_for_press_and_release() {
    let press = |key| {
        key_down(&KeyPress {
            key,
            mods: Mods::default(),
        })
    };
    assert_eq!(press(PANIC_KEY), Some(KeyAction::PanicDown));
    assert!(is_panic_key(PANIC_KEY));
    let upper = PANIC_KEY.to_ascii_uppercase();
    assert_eq!(press(&upper), Some(KeyAction::PanicDown));
    assert!(is_panic_key(&upper));
    assert!(!is_panic_key("space"));
}

#[test]
fn a_held_key_acts_once_until_it_is_released() {
    let plain = |key| KeyPress {
        key,
        mods: Mods::default(),
    };
    let play = Some(KeyAction::Ui(UiAction::TogglePlay));
    let mut keys = HeldKeys::new();

    // X11 (gpui 0.2.2) reports every repeat as a fresh press: only the
    // first acts until the key is released.
    assert_eq!(keys.press(&plain("space"), false), play);
    assert_eq!(keys.press(&plain("space"), false), None);
    assert_eq!(keys.press(&plain("space"), false), None);
    // Another key acts meanwhile, and the first stays down.
    assert_eq!(
        keys.press(&plain("f"), false),
        Some(KeyAction::Ui(UiAction::ToggleFade))
    );
    assert_eq!(keys.press(&plain("space"), false), None);
    keys.release("space");
    assert_eq!(keys.press(&plain("space"), false), play);
    // Letters count as one key in either case.
    assert_eq!(keys.press(&plain("F"), false), None);
    keys.release("F");
    assert_eq!(
        keys.press(&plain("f"), false),
        Some(KeyAction::Ui(UiAction::ToggleFade))
    );

    // macOS and Windows mark their repeats: never acted on.
    keys.release("space");
    assert_eq!(keys.press(&plain("space"), true), None);

    // After focus loss every key counts as up, since their releases went
    // elsewhere.
    assert_eq!(keys.press(&plain("space"), false), play);
    keys.forget_all();
    assert_eq!(keys.press(&plain("space"), false), play);

    // A key without a control never counts as down.
    assert_eq!(keys.press(&plain("q"), false), None);
}

#[test]
fn held_keys_leave_the_panic_key_and_shortcuts_to_their_own_rules() {
    let plain = |key| KeyPress {
        key,
        mods: Mods::default(),
    };
    let secondary = Mods {
        platform: SECONDARY_IS_PLATFORM,
        control: !SECONDARY_IS_PLATFORM,
        ..Mods::default()
    };
    let mut keys = HeldKeys::new();

    // Every press of the panic key reaches the hold, which ignores a press
    // while held; so a release the window never saw cannot swallow the
    // next panic press.
    for _ in 0..3 {
        assert_eq!(
            keys.press(&plain(PANIC_KEY), false),
            Some(KeyAction::PanicDown)
        );
    }
    // A platform repeat of it is still ignored.
    assert_eq!(keys.press(&plain(PANIC_KEY), true), None);

    // Shortcuts with a modifier are not tracked: macOS drops the release of
    // a key let go while Command is down.
    let open = KeyPress {
        key: "o",
        mods: secondary,
    };
    assert_eq!(keys.press(&open, false), Some(KeyAction::OpenSong));
    assert_eq!(keys.press(&open, false), Some(KeyAction::OpenSong));
    assert_eq!(keys.press(&open, true), None);
    // Cmd-S out of habit, its release dropped: the next plain S still stops.
    let save = KeyPress {
        key: "s",
        mods: secondary,
    };
    assert_eq!(keys.press(&save, false), None);
    assert_eq!(
        keys.press(&plain("s"), false),
        Some(KeyAction::Ui(UiAction::Stop))
    );
    let quit = KeyPress {
        key: "q",
        mods: secondary,
    };
    assert_eq!(keys.press(&quit, false), Some(KeyAction::Quit));
}

#[test]
fn a_plain_key_released_while_command_is_held_still_acts_next_time() {
    let plain = |key| KeyPress {
        key,
        mods: Mods::default(),
    };
    let command = Mods {
        platform: true,
        ..Mods::default()
    };
    let play = Some(KeyAction::Ui(UiAction::TogglePlay));
    let mut keys = HeldKeys::on(KeyPlatform::Mac);
    assert_eq!(keys.press(&plain("space"), false), play);
    // Command goes down while Space is held; Space's key-up while Command
    // is held never arrives (macOS drops it); then Command goes up.
    keys.modifiers_changed(command);
    keys.modifiers_changed(Mods::default());
    assert_eq!(
        keys.press(&plain("space"), false),
        play,
        "the next Space acts"
    );

    // Other modifiers keep the keys that are down: an X11 repeat of a held
    // key with Shift or Ctrl going down is still ignored.
    assert_eq!(
        keys.press(&plain("f"), false),
        Some(KeyAction::Ui(UiAction::ToggleFade))
    );
    keys.modifiers_changed(Mods {
        control: true,
        alt: true,
        ..Mods::default()
    });
    keys.modifiers_changed(Mods::default());
    assert_eq!(keys.press(&plain("f"), false), None, "F is still down");
}

#[test]
fn elsewhere_the_super_key_keeps_the_keys_that_are_down() {
    let plain = |key| KeyPress {
        key,
        mods: Mods::default(),
    };
    let super_key = Mods {
        platform: true,
        ..Mods::default()
    };
    // Windows and X11 report every release, so a key held through a Super
    // or Windows key press stays down: its repeats are still ignored.
    let mut keys = HeldKeys::on(KeyPlatform::Other);
    assert_eq!(
        keys.press(&plain("space"), false),
        Some(KeyAction::Ui(UiAction::TogglePlay))
    );
    keys.modifiers_changed(super_key);
    keys.modifiers_changed(Mods::default());
    assert_eq!(
        keys.press(&plain("space"), false),
        None,
        "Space is still down"
    );
    keys.release("space");
    assert_eq!(
        keys.press(&plain("space"), false),
        Some(KeyAction::Ui(UiAction::TogglePlay))
    );
}

#[test]
fn the_key_platform_here_is_this_one() {
    let expected = if cfg!(target_os = "macos") {
        KeyPlatform::Mac
    } else {
        KeyPlatform::Other
    };
    assert_eq!(KeyPlatform::HERE, expected);
}

#[test]
fn section_families_and_short_labels_come_from_the_label() {
    let expected = [
        ("Count-off", SectionKind::Neutral, "CO"),
        ("Intro", SectionKind::Neutral, "In"),
        ("Verse 2", SectionKind::Verse, "V2"),
        ("Pre-chorus", SectionKind::PreChorus, "PC"),
        ("Chorus 3", SectionKind::Chorus, "C3"),
        ("Post-chorus", SectionKind::Chorus, "PoC"),
        ("Bridge", SectionKind::Bridge, "B"),
        ("Instrumental", SectionKind::Bridge, "Ins"),
        ("Interlude", SectionKind::Bridge, "Int"),
        ("Breakdown", SectionKind::Bridge, "Bd"),
        ("Tag 1", SectionKind::Tag, "T1"),
        ("Vamp", SectionKind::Tag, "Vmp"),
        ("Turnaround", SectionKind::Neutral, "TA"),
        ("Outro", SectionKind::Neutral, "Out"),
        ("Ending", SectionKind::Neutral, "End"),
    ];
    for (label, kind, short) in expected {
        assert_eq!(SectionKind::of(label), kind, "{label}");
        assert_eq!(short_label(label), short, "{label}");
    }
}

// The design canvas's Main artboard: what the header, the song map, the
// Now and Next cards and the pads show beyond the session's own words.

/// Frames per bar at 120 BPM in 4/4 at [`RATE`].
const BAR: u64 = 96_000;

/// Intro (8 bars), Verse 1 (16), Chorus (16), Turnaround (2), the Chorus
/// again, Tag (4) and Outro (8), at 120 BPM in 4/4. Each section ends where
/// the next one in the song starts, except the Outro, the song's last.
fn bars_song() -> SongInfo {
    let position = |id: &str, label: &str, start_bar: u64, bars: u64| PositionInfo {
        section_id: id.to_owned(),
        label: label.to_owned(),
        start_frame: (start_bar - 1) * BAR,
        length_frames: bars * BAR,
        start_bar,
    };
    SongInfo {
        title: "First Light".to_owned(),
        artist: None,
        sample_rate: RATE,
        arrangement: "default".to_owned(),
        positions: vec![
            position("intro", "Intro", 1, 8),
            position("verse", "Verse 1", 9, 16),
            position("chorus", "Chorus", 25, 16),
            position("turn", "Turnaround", 41, 2),
            position("chorus", "Chorus", 25, 16),
            position("tag", "Tag", 43, 4),
            position("outro", "Outro", 47, 8),
        ],
        warnings: Vec::new(),
    }
}

/// A `song.json` with `key`, the tempo changes `tempo` and the time
/// signature changes `meters`, and the arrangements `default` ("Default")
/// and `long` ("Long ending").
fn manifest(key: Option<&str>, tempo: &[(u64, f64)], meters: &[(u64, u32, u32)]) -> String {
    let key = key.map_or_else(String::new, |key| format!("\"key\": \"{key}\","));
    let tempo: Vec<String> = tempo
        .iter()
        .map(|(at, bpm)| format!("{{\"at\": {at}, \"bpm\": {bpm}}}"))
        .collect();
    let meters: Vec<String> = meters
        .iter()
        .map(|(at, numerator, denominator)| {
            format!("{{\"at\": {at}, \"numerator\": {numerator}, \"denominator\": {denominator}}}")
        })
        .collect();
    format!(
        r#"{{
            "formatVersion": 1,
            "title": "First Light",
            "sampleRate": 48000,
            {key}
            "tempoMap": [{}],
            "timeSignatureMap": [{}],
            "stems": [],
            "sections": [],
            "arrangements": [
                {{"id": "default", "name": "Default", "sections": []}},
                {{"id": "long", "name": "Long ending", "sections": []}}
            ]
        }}"#,
        tempo.join(", "),
        meters.join(", ")
    )
}

/// The facts of [`bars_song`]: 120 BPM in 4/4 throughout.
fn bars_facts() -> SongFacts {
    SongFacts::from_manifest(&manifest(Some("G"), &[(0, 120.0)], &[(0, 4, 4)]))
        .expect("the manifest parses")
}

/// Playing bar `bar` of arrangement position `position` of [`bars_song`].
fn at_bar(position: usize, bar: u64) -> Snapshot {
    let song = bars_song();
    let info = &song.positions[position];
    let mut snapshot = snapshot();
    snapshot.position = position;
    snapshot.section = info.label.clone();
    snapshot.bar = bar;
    snapshot.beat = 1;
    snapshot.source_frame = info.start_frame + (bar - info.start_bar) * BAR;
    snapshot
}

#[test]
fn a_tag_without_a_number_keeps_its_name_and_repeated_sections_are_numbered() {
    assert_eq!(short_label("Tag"), "Tag");
    assert_eq!(short_label("Tag 2"), "T2");

    let view = PerformView::new(&at_bar(1, 10), &bars_song());
    let shorts: Vec<&str> = view.map.blocks.iter().map(|b| b.short.as_str()).collect();
    assert_eq!(shorts, ["In", "V1", "C1", "TA", "C2", "Tag", "Out"]);
    let abbreviations: Vec<&str> = view.pads.iter().map(|p| p.abbr.as_str()).collect();
    assert_eq!(abbreviations, shorts);
    // The pads keep the section's full name.
    assert_eq!(view.pads[4].label, "Chorus");
}

#[test]
fn each_section_family_has_the_design_canvas_icon() {
    let expected = [
        ("Count-off", "metronome"),
        ("Intro", "enter"),
        ("Verse 2", "lines"),
        ("Pre-chorus", "rise"),
        ("Chorus 3", "star"),
        ("Post-chorus", "star"),
        ("Bridge 1", "bridge"),
        ("Instrumental", "note"),
        ("Interlude", "note"),
        ("Breakdown", "bridge"),
        ("Tag", "tag"),
        ("Vamp", "loop"),
        ("Turnaround", "turn"),
        ("Outro", "exit"),
        ("Ending", "flag"),
    ];
    for (label, icon) in expected {
        assert_eq!(section_icon(label), icon, "{label}");
    }
    let view = PerformView::new(&at_bar(1, 10), &bars_song());
    let icons: Vec<&str> = view.pads.iter().map(|p| p.icon).collect();
    assert_eq!(
        icons,
        ["enter", "lines", "star", "turn", "star", "tag", "exit"]
    );
}

#[test]
fn sections_count_their_bars_and_the_ruler_numbers_the_bars_in_play_order() {
    let song = bars_song();
    let view = PerformView::with_facts(&at_bar(1, 10), &song, &bars_facts());
    let bars: Vec<Option<u64>> = view.map.blocks.iter().map(|b| b.bars).collect();
    assert_eq!(
        bars,
        [
            Some(8),
            Some(16),
            Some(16),
            Some(2),
            Some(16),
            Some(4),
            Some(8)
        ]
    );
    let texts: Vec<Option<&str>> = view.pads.iter().map(|p| p.bars.as_deref()).collect();
    assert_eq!(
        texts,
        [
            Some("8 bars"),
            Some("16 bars"),
            Some("16 bars"),
            Some("2 bars"),
            Some("16 bars"),
            Some("4 bars"),
            Some("8 bars")
        ]
    );
    // The bar each block starts at, counted in play order; a block shorter
    // than 4 bars has no room for its number.
    let ruler: Vec<Option<&str>> = view.map.blocks.iter().map(|b| b.ruler.as_deref()).collect();
    assert_eq!(
        ruler,
        [
            Some("1"),
            Some("9"),
            Some("25"),
            None,
            Some("43"),
            Some("59"),
            Some("63")
        ]
    );

    // Without the song's tempo, the song's last section cannot be counted:
    // no other section starts where it ends.
    let view = PerformView::new(&at_bar(1, 10), &song);
    assert_eq!(view.map.blocks[6].bars, None);
    assert_eq!(view.pads[6].bars, None);
    assert_eq!(view.map.blocks[6].ruler, None);
    assert_eq!(view.map.blocks[5].ruler.as_deref(), Some("59"));

    // A one-bar section says "bar".
    let mut short = song.clone();
    short.positions[6].length_frames = BAR;
    let view = PerformView::with_facts(&at_bar(1, 10), &short, &bars_facts());
    assert_eq!(view.pads[6].bars.as_deref(), Some("1 bar"));

    // A tempo change counts the last section at the tempo it starts in:
    // 60 BPM in 3/4 from bar 47 makes a bar 3 s, 144 000 frames.
    let facts = SongFacts::from_manifest(&manifest(
        None,
        &[(0, 120.0), (46 * BAR, 60.0)],
        &[(0, 4, 4), (46 * BAR, 3, 4)],
    ))
    .unwrap();
    let mut slow = song;
    slow.positions[6].length_frames = 4 * 144_000;
    let view = PerformView::with_facts(&at_bar(1, 10), &slow, &facts);
    assert_eq!(view.pads[6].bars.as_deref(), Some("4 bars"));
}

#[test]
fn now_and_next_show_their_section_icon_and_the_bars_left() {
    let song = bars_song();
    let facts = bars_facts();
    // Bar 30 of the Chorus, bars 25 to 40.
    let view = PerformView::with_facts(&at_bar(2, 30), &song, &facts);
    assert_eq!(view.now.name, "Chorus");
    assert_eq!(view.now.kind, SectionKind::Chorus);
    assert_eq!(view.now.icon, "star");
    assert_eq!(view.now.bars_left.as_deref(), Some("11 bars left"));
    assert_eq!(view.next.name, "Turnaround");
    assert_eq!(view.next.kind, SectionKind::Neutral);
    assert_eq!(view.next.icon, "turn");
    assert_eq!(
        view.next.bars_detail.as_deref(),
        Some("Plays in 11 bars · tap a section to jump")
    );

    // The section's last bar.
    let view = PerformView::with_facts(&at_bar(2, 40), &song, &facts);
    assert_eq!(view.now.bars_left.as_deref(), Some("1 bar left"));

    // The infinite loop repeats it.
    let mut snapshot = at_bar(2, 30);
    snapshot.infinite = true;
    let view = PerformView::with_facts(&snapshot, &song, &facts);
    assert_eq!(view.next.icon, "star");
    assert_eq!(
        view.next.bars_detail.as_deref(),
        Some("Repeats in 11 bars · infinite loop")
    );

    // A queued jump says when in time; the bars give way to it.
    let mut snapshot = at_bar(2, 30);
    snapshot.queued = Some(QueuedView {
        target: 5,
        target_label: "Tag".to_owned(),
        kind: JumpKind::Jump,
        executes_at: 0,
        frames_until: 48_000,
        deferrals: 0,
    });
    let view = PerformView::with_facts(&snapshot, &song, &facts);
    assert_eq!(view.next.icon, "tag");
    assert_eq!(view.next.kind, SectionKind::Tag);
    assert_eq!(view.next.bars_detail, None);
    assert_eq!(view.next.detail, "Executes in 1.0 s");

    // The song's last section, then the end of the song.
    let view = PerformView::with_facts(&at_bar(6, 50), &song, &facts);
    assert_eq!(view.now.icon, "exit");
    assert_eq!(view.now.bars_left.as_deref(), Some("5 bars left"));
    assert_eq!(view.next.name, "End of song");
    assert_eq!(view.next.icon, "flag");
    assert_eq!(view.next.kind, SectionKind::Neutral);
    assert_eq!(view.next.bars_detail.as_deref(), Some("Ends in 5 bars"));

    // Bars that cannot be counted leave the time in seconds.
    let view = PerformView::new(&at_bar(6, 50), &song);
    assert_eq!(view.now.bars_left, None);
    assert_eq!(view.next.bars_detail, None);
    assert_eq!(view.now.detail, "10.0 s left");
}

#[test]
fn the_song_header_shows_key_tempo_meter_and_the_arrangement_by_name() {
    let song = bars_song();
    let facts = SongFacts::from_manifest(&manifest(
        Some("G"),
        &[(0, 104.0), (BAR, 72.5)],
        &[(0, 6, 8), (BAR, 4, 4)],
    ))
    .unwrap();
    // The tempo and meter the song starts in.
    let header = PerformView::with_facts(&snapshot(), &song, &facts).header;
    assert_eq!(header.key.as_deref(), Some("G"));
    assert_eq!(header.tempo.as_deref(), Some("104 BPM"));
    assert_eq!(header.meter.as_deref(), Some("6/8"));
    assert_eq!(header.arrangement, "Default");

    let mut long = song.clone();
    long.arrangement = "long".to_owned();
    let facts = SongFacts::from_manifest(&manifest(None, &[(0, 72.5)], &[(0, 4, 4)])).unwrap();
    let header = PerformView::with_facts(&snapshot(), &long, &facts).header;
    assert_eq!(header.key, None);
    assert_eq!(header.tempo.as_deref(), Some("72.5 BPM"));
    assert_eq!(header.arrangement, "Long ending");

    // Without the manifest: the arrangement's id, and nothing else.
    let header = PerformView::new(&snapshot(), &song).header;
    assert_eq!(header.key, None);
    assert_eq!(header.tempo, None);
    assert_eq!(header.meter, None);
    assert_eq!(header.arrangement, "default");

    // A click-only tile: its tempo and meter.
    let header = PerformView::with_facts(&snapshot(), &song, &SongFacts::click(70.0, 6, 8)).header;
    assert_eq!(header.key, None);
    assert_eq!(header.tempo.as_deref(), Some("70 BPM"));
    assert_eq!(header.meter.as_deref(), Some("6/8"));
    assert_eq!(header.arrangement, "Default");

    // A manifest that does not parse gives no facts.
    assert_eq!(SongFacts::from_manifest("{"), None);
}

#[test]
fn the_device_chip_names_the_device_its_outputs_buffer_and_latency() {
    let mut snapshot = snapshot();
    let device = PerformView::new(&snapshot, &song()).device;
    assert_eq!(device.name, "Speakers");
    assert_eq!(device.meta, "2 out · 256 · 5.3 ms");
    assert_eq!(device.level, Level::Ok);

    // Underruns show on it.
    snapshot.underruns = 3;
    let device = PerformView::new(&snapshot, &song()).device;
    assert_eq!(device.meta, "2 out · 256 · 5.3 ms · 3 underruns");
    assert_eq!(device.level, Level::Warning);
    snapshot.underruns = 1;
    let device = PerformView::new(&snapshot, &song()).device;
    assert_eq!(device.meta, "2 out · 256 · 5.3 ms · 1 underrun");

    // No device: it says so, and the status says why.
    snapshot.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::Unplugged,
        reopen_error: None,
    };
    let view = PerformView::new(&snapshot, &song());
    assert_eq!(view.device.name, "Speakers");
    assert_eq!(view.device.meta, "Disconnected");
    assert_eq!(view.device.level, Level::Error);
    assert_eq!(view.status.device_level, Level::Error);
    snapshot.device = DeviceStatus::Disconnected {
        device: "Speakers".to_owned(),
        reason: LossReason::Reconfigured,
        reopen_error: Some(OutputError::NoDevice),
    };
    let device = PerformView::new(&snapshot, &song()).device;
    assert_eq!(device.meta, "Refuses the new settings");
    assert_eq!(device.level, Level::Error);
    snapshot.device = DeviceStatus::PlayerLost {
        device: "Speakers".to_owned(),
    };
    let device = PerformView::new(&snapshot, &song()).device;
    assert_eq!(device.meta, "Disconnected");
    assert_eq!(device.level, Level::Error);
}

#[test]
fn the_clock_names_the_transport_state_only_while_it_changes() {
    let mut snapshot = snapshot();
    let expected = [
        (TransportState::Stopped, None),
        (TransportState::Starting, Some("Loading…")),
        (TransportState::Playing, None),
        (TransportState::Pausing, None),
        (TransportState::Paused, None),
        (TransportState::Stopping, Some("Stopping")),
        (TransportState::Panicking, Some("Panic")),
    ];
    for (transport, note) in expected {
        snapshot.transport = transport;
        let view = PerformView::new(&snapshot, &song());
        assert_eq!(view.transport.note.as_deref(), note, "{transport:?}");
    }
}

#[test]
fn pads_share_one_row_when_they_fit_and_get_compact_past_twelve() {
    // 11 pads of at least 100 points and 8 between them fit in 1 180.
    assert_eq!(
        pad_grid(1_250.0, 11),
        PadGrid {
            columns: 11,
            height: 100.0,
            compact: false
        }
    );
    // Fewer pads share the row's width.
    assert_eq!(pad_grid(1_250.0, 8).columns, 8);
    // A narrower row wraps them.
    assert_eq!(pad_grid(700.0, 11).columns, 6);
    // More than twelve: shorter pads of at least 92 points.
    assert_eq!(
        pad_grid(1_250.0, 14),
        PadGrid {
            columns: 12,
            height: 46.0,
            compact: true
        }
    );
    // Always one column, even with no room or no pads.
    assert_eq!(pad_grid(50.0, 3).columns, 1);
    assert_eq!(pad_grid(1_250.0, 0).columns, 1);
}
