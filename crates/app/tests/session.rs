//! S1: the `app` API, driven through the fake output so the audio thread runs
//! only when a test calls it. Unless a test says otherwise, the disk keeps
//! up: before every callback the test waits until the disk thread has served
//! every request it may serve.

mod common;

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use app::testing::{FAKE_DEVICE, FAKE_DEVICE_ID, FakeOutput};
use app::{
    Command, DeviceChoice, DeviceStatus, FadeState, JumpKind, JumpTiming, LoadError, LossReason,
    OpenError, OutputError, OutputRequest, RuntimeError, Session, SessionOptions, Snapshot,
    StreamInfo, TransportState,
};
use common::{
    BAR, POS_CHORUS, POS_OUTRO, RATE, bits, closed_gate, is_silent, offline, open, play_for, start,
    step, write_song,
};
use domain::DEFAULT_SEAM_CROSSFADE;
use engine::live::DECLICK_FRAMES;
use library::Gate;

fn song() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), RATE);
    dir
}

fn options(buffer_frames: u32) -> SessionOptions {
    SessionOptions {
        buffer_frames,
        ..SessionOptions::default()
    }
}

#[test]
fn a_session_plays_the_arrangement_exactly_as_the_offline_render() {
    let dir = song();
    let expected = offline(dir.path(), "jumpy", DEFAULT_SEAM_CROSSFADE);
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            arrangement: "jumpy".to_owned(),
            ..options(256)
        },
    );

    let (first, _) = start(&mut session, &fake, 256);
    let frames = expected.len() / 2;
    let (rest, last) = play_for(&mut session, &fake, 256, frames + 1_024);
    let played = [first, rest].concat();

    assert_eq!(bits(&played[..expected.len()]), bits(&expected));
    assert!(
        is_silent(&played[expected.len()..]),
        "silence after the end"
    );
    assert_eq!(last.underruns, 0);
    assert_eq!(last.transport, TransportState::Stopped, "the end stops");
    assert_eq!(last.last_error, None);
}

#[test]
fn the_seam_crossfade_option_reaches_both_the_player_and_the_disk_stream() {
    let dir = song();
    let seam_crossfade = 6_000;
    let expected = offline(dir.path(), "jumpy", seam_crossfade);
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            arrangement: "jumpy".to_owned(),
            seam_crossfade,
            ..options(1_024)
        },
    );

    let (first, _) = start(&mut session, &fake, 1_024);
    let (rest, last) = play_for(&mut session, &fake, 1_024, expected.len() / 2);
    let played = [first, rest].concat();

    assert_eq!(
        last.underruns, 0,
        "the stream keeps what the fading-out tails read"
    );
    assert_eq!(bits(&played[..expected.len()]), bits(&expected));
}

#[test]
fn a_song_whose_rate_is_too_low_for_the_largest_buffer_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), 1_000);
    let fake = FakeOutput::new();

    let error = Session::open(dir.path(), Box::new(fake.clone()), options(64))
        .err()
        .expect("the window ahead of the playhead is 1 000 frames");

    assert!(matches!(
        error,
        OpenError::RateTooLow {
            sample_rate: 1_000,
            ahead_frames: 1_000
        }
    ));
    assert_eq!(
        error.to_string(),
        "the song's sample rate (1000 Hz) is too low to stream: 1000 frames kept ahead of the playhead do not cover a buffer of 1024 frames"
    );
    assert!(fake.opened().is_empty(), "no device was opened");
}

#[test]
fn buffer_sizes_outside_64_to_1024_frames_are_rejected() {
    let dir = song();
    for frames in [0, 63, 1_025, 4_096] {
        let fake = FakeOutput::new();
        let error = Session::open(dir.path(), Box::new(fake.clone()), options(frames))
            .err()
            .expect("the size is out of range");
        assert!(matches!(error, OpenError::BufferSize { frames: f } if f == frames));
        assert_eq!(
            error.to_string(),
            format!("the buffer size must be 64 to 1024 frames, not {frames}")
        );
        assert!(fake.opened().is_empty());
    }
    for frames in [64, 1_024] {
        let fake = FakeOutput::new();
        let mut session = open(dir.path(), &fake, options(frames));
        assert_eq!(fake.opened()[0].buffer_frames, frames);
        let DeviceStatus::Connected(info) = session.poll().device else {
            panic!("the device plays");
        };
        assert_eq!(info.buffer_frames, frames);
        assert_eq!(
            info.latency(),
            Duration::from_secs_f64(f64::from(frames) / f64::from(RATE))
        );
    }
}

#[test]
fn play_from_a_cold_stream_waits_for_the_disk_so_the_first_buffer_is_not_silent() {
    let dir = song();
    let expected = offline(dir.path(), "default", DEFAULT_SEAM_CROSSFADE);
    let (gate, open_gate) = closed_gate();
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(gate),
            ..options(256)
        },
    );

    session.send(Command::Play).unwrap();
    assert_eq!(session.poll().transport, TransportState::Starting);
    for _ in 0..8 {
        let out = fake.render(256).unwrap();
        assert!(is_silent(&out));
        let snapshot = session.poll();
        assert_eq!(snapshot.transport, TransportState::Starting);
        assert_eq!(snapshot.output_frame, 0, "the transport did not start");
        assert_eq!(snapshot.underruns, 0, "nothing tried to play");
    }

    open_gate.store(true, Ordering::SeqCst);
    let monitor = session.disk_monitor();
    assert!(monitor.wait_idle(common::IDLE));
    assert!(is_silent(&fake.render(256).unwrap()), "still waiting");
    assert_eq!(session.poll().transport, TransportState::Starting);
    let first = fake.render(256).unwrap();
    let snapshot = session.poll();

    assert_eq!(snapshot.transport, TransportState::Playing);
    assert_eq!(snapshot.output_frame, 256);
    assert_eq!(
        bits(&first),
        bits(&expected[..512]),
        "the song's first buffer"
    );
    assert_eq!(snapshot.underruns, 0);
}

#[test]
fn play_gives_up_with_a_clear_error_when_the_disk_does_not_deliver_in_time() {
    let dir = song();
    let (gate, open_gate) = closed_gate();
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(gate),
            start_timeout: Duration::from_millis(50),
            ..options(256)
        },
    );

    session.send(Command::Play).unwrap();
    fake.render(256).unwrap();
    assert_eq!(session.poll().transport, TransportState::Starting);
    thread::sleep(Duration::from_millis(60));
    let snapshot = session.poll();

    assert_eq!(snapshot.transport, TransportState::Stopped);
    let error = RuntimeError::StartTimeout {
        waited: Duration::from_millis(50),
    };
    assert_eq!(snapshot.last_error, Some(error.clone()));
    assert_eq!(
        error.to_string(),
        "the audio at the playhead did not load from disk within 0.1 s; playback did not start"
    );

    // The play was dropped: audio arriving later does not start the song.
    open_gate.store(true, Ordering::SeqCst);
    let monitor = session.disk_monitor();
    let (out, snapshot) = step(&mut session, &fake, &monitor, 256);
    assert!(is_silent(&out));
    assert_eq!(snapshot.transport, TransportState::Stopped);
    let (out, snapshot) = step(&mut session, &fake, &monitor, 256);
    assert!(is_silent(&out));
    assert_eq!(snapshot.transport, TransportState::Stopped);
}

/// A gate the disk thread stays inside while `hold` is set.
fn blocking_gate(hold: &Arc<AtomicBool>, entered: &Arc<AtomicBool>) -> Gate {
    let (hold, entered) = (Arc::clone(hold), Arc::clone(entered));
    Arc::new(move |_: &str, _| {
        entered.store(true, Ordering::SeqCst);
        while hold.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        true
    })
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn closing_a_session_drops_the_player_on_the_control_thread_not_the_audio_thread() {
    let dir = song();
    let (hold, entered) = (
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
    );
    let fake = FakeOutput::new();
    fake.drop_renderers_on_audio_thread();
    let session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(blocking_gate(&hold, &entered)),
            ..options(256)
        },
    );
    let monitor = session.disk_monitor();
    wait_for("the disk thread to enter the gate", || {
        entered.load(Ordering::SeqCst)
    });

    // Dropping the player joins the disk thread, which is stuck in the gate.
    let control = thread::spawn(move || drop(session));
    let renderer_dropped = {
        let deadline = Instant::now() + Duration::from_secs(1);
        while fake.renderers_dropped_on_audio_thread() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        fake.renderers_dropped_on_audio_thread() == 1
    };
    let control_was_waiting = !control.is_finished();
    let disk_was_running = monitor.is_running();
    hold.store(false, Ordering::SeqCst);
    control.join().unwrap();

    assert!(
        renderer_dropped,
        "the audio thread dropped its renderer without waiting for the disk thread"
    );
    assert!(
        control_was_waiting && disk_was_running,
        "the control thread is the one that waits for the disk thread"
    );
    assert!(
        !monitor.is_running(),
        "closing the session ends the disk thread"
    );
}

#[test]
fn the_device_opens_at_the_songs_sample_rate_or_the_session_says_why_not() {
    let dir = song();
    let fake = FakeOutput::without_devices().with_device("Studio", 8, Some(&[44_100, 48_000]));

    let error = Session::open(dir.path(), Box::new(fake.clone()), options(256))
        .err()
        .expect("the device does not run at 8 kHz");

    assert!(matches!(
        &error,
        OpenError::Output(OutputError::UnsupportedSampleRate {
            device,
            requested: RATE,
            ..
        }) if device == "Studio"
    ));
    assert_eq!(
        error.to_string(),
        "audio device \"Studio\" cannot play at 8000 Hz, the song's sample rate (nothing resamples); it supports 44100 Hz, 48000 Hz"
    );

    let fake = FakeOutput::without_devices().with_device("Studio", 8, Some(&[RATE]));
    let mut session = open(dir.path(), &fake, options(256));
    assert_eq!(
        fake.opened(),
        vec![OutputRequest {
            device: None,
            sample_rate: RATE,
            buffer_frames: 256,
            channels: 2
        }]
    );
    assert_eq!(
        session.poll().device,
        DeviceStatus::Connected(StreamInfo {
            device: "Studio".to_owned(),
            device_id: "fake:1".to_owned(),
            sample_rate: RATE,
            buffer_frames: 256,
            // A stereo session opens two of the eight outputs.
            channels: 2
        })
    );
}

/// A gate that holds back the blocks reaching into source frames `from..`
/// until the returned flag is set.
fn gate_from(from: u64) -> (Gate, Arc<AtomicBool>) {
    let open = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&open);
    let gate: Gate = Arc::new(move |_: &str, block: std::ops::Range<u64>| {
        block.end <= from || flag.load(Ordering::SeqCst)
    });
    (gate, open)
}

#[test]
fn a_jump_waiting_for_its_audio_shows_as_the_last_error() {
    let dir = song();
    let (gate, _never) = gate_from(6 * BAR);
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(gate),
            ..options(1_024)
        },
    );
    start(&mut session, &fake, 1_024);
    session
        .send(Command::QueueJump {
            position: POS_OUTRO,
        })
        .unwrap();

    let (_, before) = play_for(&mut session, &fake, 1_024, 30_000);
    assert_eq!(before.queued.as_ref().unwrap().deferrals, 0);
    assert_eq!(before.last_error, None);
    let (_, after) = play_for(&mut session, &fake, 1_024, 4_096);

    assert_eq!(
        after.position, 1,
        "the arrangement played on into the verse"
    );
    let queued = after.queued.as_ref().unwrap();
    assert_eq!((queued.target, queued.deferrals), (POS_OUTRO, 1));
    let error = RuntimeError::JumpWaiting {
        section: "Outro".to_owned(),
        deferrals: 1,
    };
    assert_eq!(after.last_error, Some(error.clone()));
    assert_eq!(
        error.to_string(),
        "the jump to Outro is waiting for its audio from disk; it has moved 1 boundary(ies) on"
    );
}

#[test]
fn the_waiting_jump_error_clears_once_the_jump_executes_or_is_cancelled() {
    for cancel in [false, true] {
        let dir = song();
        let (gate, open_gate) = gate_from(6 * BAR);
        let fake = FakeOutput::new();
        let mut session = open(
            dir.path(),
            &fake,
            SessionOptions {
                disk_gate: Some(gate),
                ..options(1_024)
            },
        );
        start(&mut session, &fake, 1_024);
        session
            .send(Command::QueueJump {
                position: POS_OUTRO,
            })
            .unwrap();
        let (_, waiting) = play_for(&mut session, &fake, 1_024, 2 * BAR as usize + 1_024);
        assert_eq!(waiting.position, 1, "the jump moved on to the verse's end");
        assert!(matches!(
            waiting.last_error,
            Some(RuntimeError::JumpWaiting { deferrals: 1, .. })
        ));

        let done = if cancel {
            session.send(Command::CancelJump).unwrap();
            let (_, cancelled) = play_for(&mut session, &fake, 1_024, 1_024);
            assert_eq!(cancelled.position, 1);
            cancelled
        } else {
            open_gate.store(true, Ordering::SeqCst);
            let (_, jumped) = play_for(&mut session, &fake, 1_024, 2 * BAR as usize);
            assert_eq!(jumped.position, POS_OUTRO, "the jump executed");
            jumped
        };
        assert_eq!(done.queued, None);
        assert_eq!(done.last_error, None, "cancelled: {cancel}");
    }
}

#[test]
fn a_start_timeout_clears_once_play_starts() {
    let dir = song();
    let (gate, open_gate) = closed_gate();
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(gate),
            start_timeout: Duration::from_millis(50),
            ..options(256)
        },
    );
    session.send(Command::Play).unwrap();
    fake.render(256).unwrap();
    thread::sleep(Duration::from_millis(60));
    let timed_out = session.poll();
    assert_eq!(timed_out.transport, TransportState::Stopped);
    assert!(matches!(
        timed_out.last_error,
        Some(RuntimeError::StartTimeout { .. })
    ));

    open_gate.store(true, Ordering::SeqCst);
    let (_, playing) = start(&mut session, &fake, 256);

    assert_eq!(playing.transport, TransportState::Playing);
    assert_eq!(playing.last_error, None);
}

#[test]
fn a_decode_failure_stays_after_a_clean_stretch_because_failed_blocks_never_decode_again() {
    let dir = song();
    let (gate, open_gate) = gate_from(2 * BAR);
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(gate),
            ..options(1_024)
        },
    );
    // The drums lose everything from the verse on, after they were checked.
    let drums = OpenOptions::new()
        .write(true)
        .open(dir.path().join("stems/drums.wav"))
        .unwrap();
    drums.set_len(44 + 2 * 2 * BAR).unwrap();
    open_gate.store(true, Ordering::SeqCst);
    start(&mut session, &fake, 1_024);
    let (_, failing) = play_for(&mut session, &fake, 1_024, 2 * BAR as usize + 8_192);
    assert!(failing.decode_errors > 0 && failing.underruns > 0);
    assert!(matches!(
        failing.last_error,
        Some(RuntimeError::DecodeFailed { .. })
    ));

    // Back in the intro, every block decodes and every read is served.
    session.send(Command::BackToStart).unwrap();
    // Two of the disk's 1 s read-ahead windows at 8 kHz, all of it clean.
    let (_, clean) = play_for(&mut session, &fake, 1_024, 2 * 8_000);

    assert_eq!(clean.position, 0);
    assert_eq!(
        clean.decode_errors, failing.decode_errors,
        "nothing new failed"
    );
    assert!(
        matches!(clean.last_error, Some(RuntimeError::DecodeFailed { .. })),
        "the failed verse will play as silence again, so the error stays"
    );
}

#[test]
fn play_right_after_back_to_start_waits_for_a_report_about_the_new_playhead() {
    let dir = song();
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options(256));
    let monitor = session.disk_monitor();
    start(&mut session, &fake, 256);
    play_for(&mut session, &fake, 256, 3 * BAR as usize);
    session.send(Command::Pause).unwrap();
    let (_, paused) = play_for(&mut session, &fake, 256, 1_024);
    assert_eq!(
        (paused.transport, paused.position),
        (TransportState::Paused, 1)
    );

    // Both arrive before the audio thread runs again: the last report is
    // still about the verse.
    session.send(Command::BackToStart).unwrap();
    session.send(Command::Play).unwrap();
    assert_eq!(session.poll().transport, TransportState::Starting);
    let (out, moved) = step(&mut session, &fake, &monitor, 256);

    assert!(
        is_silent(&out),
        "play is not sent on a report about the old playhead"
    );
    assert_eq!(moved.transport, TransportState::Starting);
    assert_eq!((moved.position, moved.source_frame), (0, 0));
    let (out, playing) = step(&mut session, &fake, &monitor, 256);
    assert!(!is_silent(&out));
    assert_eq!(playing.transport, TransportState::Playing);
    assert_eq!(playing.source_frame, 256, "it plays from the start");
    assert_eq!(playing.underruns, 0);
}

#[test]
fn play_right_after_pause_while_playing_still_resumes_at_once() {
    let dir = song();
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options(256));
    let monitor = session.disk_monitor();
    start(&mut session, &fake, 256);
    let (_, playing) = play_for(&mut session, &fake, 256, 4_096);

    session.send(Command::Pause).unwrap();
    session.send(Command::Play).unwrap();
    let (out, snapshot) = step(&mut session, &fake, &monitor, 256);

    assert!(!is_silent(&out));
    assert_eq!(snapshot.transport, TransportState::Playing);
    assert_eq!(snapshot.source_frame, playing.source_frame + 256);
}

#[test]
fn blocks_the_disk_cannot_decode_show_as_the_last_error() {
    let dir = song();
    let (gate, open_gate) = gate_from(BAR);
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            disk_gate: Some(gate),
            ..options(1_024)
        },
    );
    // The drums file loses everything past half a bar, after it was checked.
    let drums = OpenOptions::new()
        .write(true)
        .open(dir.path().join("stems/drums.wav"))
        .unwrap();
    drums.set_len(44 + BAR).unwrap();
    open_gate.store(true, Ordering::SeqCst);

    start(&mut session, &fake, 1_024);
    let (_, snapshot) = play_for(&mut session, &fake, 1_024, 3 * BAR as usize);

    assert!(snapshot.decode_errors > 0);
    let error = RuntimeError::DecodeFailed {
        blocks: snapshot.decode_errors,
    };
    assert_eq!(snapshot.last_error, Some(error.clone()));
    assert_eq!(
        error.to_string(),
        format!(
            "the disk could not decode {} block(s) of audio; they play as silence",
            snapshot.decode_errors
        )
    );
    assert!(snapshot.underruns > 0, "the drums played silence there");
}

#[test]
fn unplugging_the_device_pauses_with_a_clear_message_and_plugging_it_back_restores_it_paused() {
    let dir = song();
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            reconnect_interval: Duration::ZERO,
            ..options(256)
        },
    );
    start(&mut session, &fake, 256);
    let (_, playing) = play_for(&mut session, &fake, 256, 4_096);
    assert_eq!(playing.transport, TransportState::Playing);

    fake.unplug(FAKE_DEVICE_ID);
    let lost = session.poll();

    assert_eq!(
        lost.device,
        DeviceStatus::Disconnected {
            device: FAKE_DEVICE.to_owned(),
            reason: LossReason::Unplugged,
            reopen_error: None,
        }
    );
    assert_eq!(lost.transport, TransportState::Paused);
    assert!(
        (playing.source_frame..=playing.source_frame + DECLICK_FRAMES).contains(&lost.source_frame),
        "it paused where it was"
    );
    let error = RuntimeError::DeviceLost {
        device: FAKE_DEVICE.to_owned(),
        reason: LossReason::Unplugged,
    };
    assert_eq!(lost.last_error, Some(error.clone()));
    assert_eq!(
        error.to_string(),
        "audio device \"Fake Output\" was unplugged; playback is paused until it is plugged back in"
    );

    // Play waits for the device; other commands apply at once.
    assert_eq!(session.send(Command::Play), Err(error));
    session
        .send(Command::QueueJump {
            position: POS_CHORUS,
        })
        .unwrap();
    let waiting = session.poll();
    assert_eq!(waiting.transport, TransportState::Paused);
    assert_eq!(waiting.queued.as_ref().unwrap().target, POS_CHORUS);
    assert!(matches!(waiting.device, DeviceStatus::Disconnected { .. }));
    assert_eq!(fake.opened().len(), 1, "the retry found no device");

    fake.plug(FAKE_DEVICE_ID);
    let back = session.poll();

    assert!(
        matches!(&back.device, DeviceStatus::Connected(info) if info.device_id == FAKE_DEVICE_ID)
    );
    assert_eq!(back.transport, TransportState::Paused, "it stays paused");
    assert_eq!(back.last_error, None);
    assert_eq!(
        fake.opened()[1],
        OutputRequest {
            device: Some(DeviceChoice::Id(FAKE_DEVICE_ID.to_owned())),
            sample_rate: RATE,
            buffer_frames: 256,
            channels: 2
        },
        "the same device, by id"
    );
    let (out, still) = play_for(&mut session, &fake, 256, 2_048);
    assert!(is_silent(&out), "nothing plays until play");
    assert_eq!(still.transport, TransportState::Paused);
    assert_eq!(still.source_frame, lost.source_frame);

    let (resumed, _) = start(&mut session, &fake, 256);
    assert!(!is_silent(&resumed));
    assert_eq!(session.poll().transport, TransportState::Playing);
}

#[test]
fn a_device_that_comes_back_but_refuses_to_open_says_why_and_the_session_keeps_trying() {
    let dir = song();
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            reconnect_interval: Duration::ZERO,
            ..options(256)
        },
    );
    start(&mut session, &fake, 256);
    fake.unplug(FAKE_DEVICE_ID);
    session.poll();

    // It comes back set to 44.1 kHz only.
    fake.set_rates(FAKE_DEVICE_ID, Some(&[44_100]));
    fake.plug(FAKE_DEVICE_ID);
    session.poll();
    let refused = session.poll();

    let reopen = OutputError::UnsupportedSampleRate {
        device: FAKE_DEVICE.to_owned(),
        requested: RATE,
        supported: vec![(44_100, 44_100)],
    };
    assert_eq!(
        refused.device,
        DeviceStatus::Disconnected {
            device: FAKE_DEVICE.to_owned(),
            reason: LossReason::Unplugged,
            reopen_error: Some(reopen.clone()),
        }
    );
    let error = RuntimeError::DeviceReopenFailed {
        device: FAKE_DEVICE.to_owned(),
        error: reopen,
    };
    assert_eq!(refused.last_error, Some(error.clone()));
    assert_eq!(
        error.to_string(),
        "audio device \"Fake Output\" cannot be opened again: audio device \"Fake Output\" cannot play at 8000 Hz, the song's sample rate (nothing resamples); it supports 44100 Hz; playback stays paused"
    );
    assert_eq!(session.send(Command::Play), Err(error));
    assert_eq!(refused.transport, TransportState::Paused);

    // Gone again: it reads as lost again, not as refused.
    fake.unplug(FAKE_DEVICE_ID);
    let gone = session.poll();
    assert!(matches!(
        gone.device,
        DeviceStatus::Disconnected {
            reopen_error: None,
            ..
        }
    ));
    assert!(matches!(
        gone.last_error,
        Some(RuntimeError::DeviceLost {
            reason: LossReason::Unplugged,
            ..
        })
    ));

    // Set back to the song's rate, it opens and every device error clears.
    fake.set_rates(FAKE_DEVICE_ID, None);
    fake.plug(FAKE_DEVICE_ID);
    let back = session.poll();
    assert!(matches!(back.device, DeviceStatus::Connected(_)));
    assert_eq!(back.last_error, None);
    assert_eq!(back.transport, TransportState::Paused);
}

#[test]
fn a_lost_device_is_found_again_by_its_id_never_by_a_name_another_device_shares() {
    let dir = song();
    let fake = FakeOutput::without_devices()
        .with_device("Twin", 2, None)
        .with_device("Twin", 2, None);
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            reconnect_interval: Duration::ZERO,
            ..options(256)
        },
    );
    start(&mut session, &fake, 256);
    fake.unplug("fake:1");
    session.poll();

    let waiting = session.poll();
    assert!(
        matches!(waiting.device, DeviceStatus::Disconnected { .. }),
        "the other interface with the same name is not taken: {:?}",
        waiting.device
    );
    assert_eq!(fake.opened().len(), 1);

    fake.plug("fake:1");
    let back = session.poll();
    assert!(matches!(&back.device, DeviceStatus::Connected(info) if info.device_id == "fake:1"));
    assert_eq!(
        fake.opened()[1].device,
        Some(DeviceChoice::Id("fake:1".to_owned()))
    );
}

#[test]
fn a_device_is_chosen_by_id_or_by_a_name_only_one_device_has() {
    let dir = song();
    let twins = || {
        FakeOutput::without_devices()
            .with_device("Studio", 2, None)
            .with_device("Twin", 2, None)
            .with_device("Twin", 2, None)
    };
    let chosen = |device: &str| {
        let fake = twins();
        Session::open(
            dir.path(),
            Box::new(fake),
            SessionOptions {
                device: Some(device.to_owned()),
                ..options(256)
            },
        )
        .map(|mut session| match session.poll().device {
            DeviceStatus::Connected(info) => info.device_id,
            other => panic!("not connected: {other:?}"),
        })
    };

    assert_eq!(chosen("fake:3").unwrap(), "fake:3");
    assert_eq!(chosen("Studio").unwrap(), "fake:1");
    assert_eq!(chosen("studio").unwrap(), "fake:1", "ASCII case is ignored");
    let error = chosen("Twin").unwrap_err();
    assert!(matches!(
        &error,
        OpenError::Output(OutputError::AmbiguousDevice { name, ids })
            if name == "Twin" && ids == &["fake:2", "fake:3"]
    ));
    assert_eq!(
        error.to_string(),
        "2 audio output devices are named \"Twin\"; choose one by id: \"fake:2\", \"fake:3\""
    );
    let error = chosen("Nowhere").unwrap_err();
    assert_eq!(
        error.to_string(),
        "no audio output device has the name or id \"Nowhere\"; available: \"Studio\", \"Twin\", \"Twin\""
    );
}

#[test]
fn the_loss_message_names_why_the_stream_stopped() {
    let dir = song();
    for (reason, message) in [
        (
            LossReason::DeviceBusy,
            "audio device \"Fake Output\" stopped: the device is busy with another application; playback is paused until the device opens again",
        ),
        (
            LossReason::StreamInvalidated,
            "audio device \"Fake Output\" stopped: the system invalidated the stream, for example because the device's sample rate changed; playback is paused until the device opens again",
        ),
    ] {
        let fake = FakeOutput::new();
        let mut session = open(dir.path(), &fake, options(256));
        start(&mut session, &fake, 256);

        fake.lose(FAKE_DEVICE_ID, reason);
        let lost = session.poll();

        assert_eq!(
            lost.device,
            DeviceStatus::Disconnected {
                device: FAKE_DEVICE.to_owned(),
                reason,
                reopen_error: None,
            }
        );
        assert_eq!(lost.transport, TransportState::Paused);
        let error = RuntimeError::DeviceLost {
            device: FAKE_DEVICE.to_owned(),
            reason,
        };
        assert_eq!(lost.last_error, Some(error.clone()));
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn the_snapshot_reports_transport_playhead_section_queue_loop_fade_and_device() {
    let dir = song();
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options(1_024));

    let fresh = session.poll();
    assert_eq!(fresh.transport, TransportState::Stopped);
    assert_eq!(
        (
            fresh.output_frame,
            fresh.source_frame,
            fresh.bar,
            fresh.beat
        ),
        (0, 0, 1, 1)
    );
    assert_eq!((fresh.position, fresh.section.as_str()), (0, "Intro"));
    assert_eq!(fresh.queued, None);
    assert_eq!(fresh.jump_timing, JumpTiming::SectionEnd);
    assert!(!fresh.infinite && !fresh.live_loop());
    assert_eq!(fresh.fade.state, FadeState::In);
    assert_eq!(fresh.fade.level, 1.0);
    assert_eq!(fresh.fade.duration, Duration::from_secs(3));
    assert_eq!(
        fresh.device,
        DeviceStatus::Connected(StreamInfo {
            device: FAKE_DEVICE.to_owned(),
            device_id: FAKE_DEVICE_ID.to_owned(),
            sample_rate: RATE,
            buffer_frames: 1_024,
            channels: 2
        })
    );
    assert_eq!((fresh.underruns, fresh.decode_errors), (0, 0));
    assert_eq!(fresh.last_error, None);

    let info = session.song();
    assert_eq!(info.title, "Session Test");
    assert_eq!(info.artist.as_deref(), Some("The Testers"));
    assert_eq!(info.arrangement, "default");
    let labels: Vec<(&str, &str, u64, u64)> = info
        .positions
        .iter()
        .map(|p| {
            (
                p.section_id.as_str(),
                p.label.as_str(),
                p.start_bar,
                p.length_frames,
            )
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            ("intro", "Intro", 1, 2 * BAR),
            ("verse", "Verse 1", 3, 2 * BAR),
            ("chorus", "Chorus", 5, 2 * BAR),
            ("outro", "Outro", 7, BAR)
        ]
    );

    start(&mut session, &fake, 1_024);
    let (_, playing) = play_for(&mut session, &fake, 1_024, 4_096);
    assert_eq!(playing.transport, TransportState::Playing);
    assert_eq!((playing.output_frame, playing.source_frame), (5_120, 5_120));
    assert_eq!(
        (playing.bar, playing.beat),
        (1, 2),
        "a beat is 4 000 frames"
    );

    session
        .send(Command::QueueJump {
            position: POS_CHORUS,
        })
        .unwrap();
    let (_, queued) = play_for(&mut session, &fake, 1_024, 1_024);
    let jump = queued.queued.clone().unwrap();
    assert_eq!(
        (jump.target, jump.target_label.as_str(), jump.kind),
        (POS_CHORUS, "Chorus", JumpKind::Jump)
    );
    assert_eq!(
        (jump.executes_at, jump.frames_until),
        (2 * BAR, 2 * BAR - 6_144)
    );
    assert_eq!(jump.deferrals, 0);

    session
        .send(Command::SetJumpTiming(JumpTiming::NextBar))
        .unwrap();
    let (_, next_bar) = play_for(&mut session, &fake, 1_024, 1_024);
    assert_eq!(next_bar.jump_timing, JumpTiming::NextBar);
    assert_eq!(next_bar.queued.unwrap().executes_at, BAR);

    session.send(Command::LiveLoop).unwrap();
    session.send(Command::SetInfinite(true)).unwrap();
    let (_, looped) = play_for(&mut session, &fake, 1_024, 1_024);
    assert!(looped.live_loop() && looped.infinite);
    assert_eq!(looped.queued.unwrap().target_label, "Intro");

    session.send(Command::CancelJump).unwrap();
    session
        .send(Command::SetFadeDuration(Duration::from_secs(1)))
        .unwrap();
    session.send(Command::ToggleFade).unwrap();
    let (_, fading) = play_for(&mut session, &fake, 1_024, 1_024);
    assert_eq!(fading.queued, None);
    assert_eq!(fading.fade.duration, Duration::from_secs(1));
    assert_eq!(fading.fade.state, FadeState::FadingOut);
    assert_eq!(fading.fade.level, 1.0 - 1_024.0 / 8_000.0);

    session.send(Command::Panic).unwrap();
    let (_, panicking) = play_for(&mut session, &fake, 1_024, 1_024);
    assert_eq!(panicking.transport, TransportState::Panicking);
    let (out, stopped) = play_for(&mut session, &fake, 1_024, 4_096);
    assert_eq!(stopped.transport, TransportState::Stopped);
    assert_eq!((stopped.output_frame, stopped.position), (0, 0));
    assert!(is_silent(&out[2 * 3_072..]), "silent once the panic ended");

    let error = RuntimeError::UnknownPosition {
        position: 9,
        positions: 4,
    };
    assert_eq!(
        session.send(Command::QueueJump { position: 9 }),
        Err(error.clone())
    );
    assert_eq!(session.poll().last_error, Some(error));
}

#[test]
fn opening_a_missing_folder_or_arrangement_fails_before_any_device_opens() {
    let fake = FakeOutput::new();
    let missing = Path::new("/nonexistent/song");
    let error = Session::open(missing, Box::new(fake.clone()), SessionOptions::default())
        .err()
        .unwrap();
    assert!(matches!(
        error,
        OpenError::Load(LoadError::ReadManifest { .. })
    ));
    // The platform joins the path (a backslash on Windows).
    let expected = format!("cannot read {}:", missing.join("song.json").display());
    assert!(error.to_string().starts_with(&expected), "{error}");

    let dir = song();
    let error = Session::open(
        dir.path(),
        Box::new(fake.clone()),
        SessionOptions {
            arrangement: "encore".to_owned(),
            ..SessionOptions::default()
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.to_string(), "unknown arrangement \"encore\"");
    assert!(fake.opened().is_empty());

    let error = Session::open(
        dir.path(),
        Box::new(FakeOutput::without_devices()),
        SessionOptions::default(),
    )
    .err()
    .unwrap();
    assert!(matches!(error, OpenError::Output(OutputError::NoDevice)));
}

/// Opens a session on the arrangement `repeat` (intro, chorus, verse,
/// chorus) whose drums lose the chorus and what follows after the session
/// checked them; the verse stays whole.
fn repeat_with_a_failing_chorus(
    fake: &FakeOutput,
    extra: SessionOptions,
) -> (tempfile::TempDir, Session) {
    let dir = song();
    let json = std::fs::read_to_string(dir.path().join("song.json")).unwrap();
    let mut package = domain::SongPackage::from_json(&json).unwrap();
    package.arrangements.push(domain::Arrangement {
        id: "repeat".to_owned(),
        name: "repeat".to_owned(),
        sections: ["intro", "chorus", "verse", "chorus"]
            .iter()
            .map(|section| (*section).to_owned())
            .collect(),
        infinite_loop: Vec::new(),
    });
    std::fs::write(dir.path().join("song.json"), package.to_json().unwrap()).unwrap();
    let (gate, open_gate) = gate_from(BAR / 2);
    let session = open(
        dir.path(),
        fake,
        SessionOptions {
            arrangement: "repeat".to_owned(),
            disk_gate: Some(gate),
            ..extra
        },
    );
    // The chorus is source bars 4 to 6.
    let drums = OpenOptions::new()
        .write(true)
        .open(dir.path().join("stems/drums.wav"))
        .unwrap();
    drums.set_len(44 + 2 * 4 * BAR).unwrap();
    open_gate.store(true, Ordering::SeqCst);
    (dir, session)
}

/// Plays to the end of the arrangement, calling `each` after every buffer,
/// and fails when a buffer adds underruns while the snapshot names no
/// problem. Returns the underruns played.
fn play_failing_song(
    session: &mut Session,
    fake: &FakeOutput,
    mut each: impl FnMut(&Snapshot, &mut Session),
) -> u64 {
    let monitor = session.disk_monitor();
    let mut underruns = 0;
    loop {
        let (_, snapshot) = step(session, fake, &monitor, 256);
        if snapshot.transport == TransportState::Stopped {
            break;
        }
        assert!(
            snapshot.underruns == underruns || snapshot.last_error.is_some(),
            "the drums play silence at frame {} (position {}) and the snapshot names no problem",
            snapshot.output_frame,
            snapshot.position
        );
        underruns = snapshot.underruns;
        each(&snapshot, session);
    }
    underruns
}

#[test]
fn a_failed_section_that_plays_again_after_a_clean_stretch_shows_the_decode_error_again() {
    let fake = FakeOutput::new();
    let (_dir, mut session) = repeat_with_a_failing_chorus(&fake, options(256));
    start(&mut session, &fake, 256);

    let underruns = play_failing_song(&mut session, &fake, |_, _| {});

    assert!(underruns > 0, "the second chorus played its failed blocks");
}

#[test]
fn cancelling_a_jump_that_waited_for_failed_audio_shows_the_decode_error_again() {
    let fake = FakeOutput::new();
    let (_dir, mut session) = repeat_with_a_failing_chorus(&fake, options(256));
    start(&mut session, &fake, 256);
    let mut queued = false;
    let mut cancelled = false;
    let mut after_cancel: Option<Option<RuntimeError>> = None;

    // In the verse, queue a jump back to the first chorus: its audio failed,
    // so the jump waits; cancel it once it does.
    let underruns = play_failing_song(&mut session, &fake, |snapshot, session| {
        if snapshot.position == 2 && !queued {
            session.send(Command::QueueJump { position: 1 }).unwrap();
            queued = true;
        }
        let waiting = matches!(snapshot.last_error, Some(RuntimeError::JumpWaiting { .. }));
        if !cancelled && waiting {
            session.send(Command::CancelJump).unwrap();
            cancelled = true;
        } else if cancelled && !waiting && after_cancel.is_none() {
            after_cancel = Some(snapshot.last_error.clone());
        }
    });

    assert!(cancelled, "the jump waited for the failed chorus");
    let after_cancel = after_cancel.expect("the waiting jump ended");
    assert!(
        matches!(after_cancel, Some(RuntimeError::DecodeFailed { .. })),
        "the decode failure is back once the jump is over: {after_cancel:?}"
    );
    assert!(underruns > 0, "the second chorus played its failed blocks");
}

#[test]
fn a_device_that_plays_again_shows_an_earlier_decode_failure_again() {
    let fake = FakeOutput::new();
    let (_dir, mut session) = repeat_with_a_failing_chorus(
        &fake,
        SessionOptions {
            reconnect_interval: Duration::ZERO,
            ..options(256)
        },
    );
    start(&mut session, &fake, 256);
    let (_, failing) = play_for(&mut session, &fake, 256, 4 * BAR as usize);
    assert!(matches!(
        failing.last_error,
        Some(RuntimeError::DecodeFailed { .. })
    ));

    fake.unplug(FAKE_DEVICE_ID);
    let lost = session.poll();
    assert!(matches!(
        lost.last_error,
        Some(RuntimeError::DeviceLost { .. })
    ));
    fake.plug(FAKE_DEVICE_ID);
    let back = session.poll();

    assert!(matches!(back.device, DeviceStatus::Connected(_)));
    assert_eq!(
        back.last_error,
        Some(RuntimeError::DecodeFailed {
            blocks: back.decode_errors
        })
    );
}
