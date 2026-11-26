//! S6 sequences from the independent verification: the view-model on a real `Session` with
//! `FakeOutput`, beyond the single-step mappings in view_model.rs.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use app::testing::{FAKE_DEVICE_ID, FakeOutput};
use app::{
    DeviceStatus, JumpKind, JumpTiming, RuntimeError, Session, SessionOptions, Snapshot, SongInfo,
    TransportState,
};
use domain::{
    Arrangement, SectionMarker, SectionType, SongPackage, Stem, StemRole, TempoPoint,
    TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use openvamp_desktop::action::{UiAction, command_for};
use openvamp_desktop::view::{PerformView, SectionKind};

const RATE: u32 = 8_000;
const BAR: u64 = 2 * RATE as u64;
const FRAMES: usize = 256;

/// Sections in source order, one bar each: intro, verse 1, verse 2, chorus,
/// outro. `order` is the default arrangement.
fn write_song(dir: &Path, order: &[&str]) {
    write_song_bars(dir, order, 1);
}

fn write_song_bars(dir: &Path, order: &[&str], bars: u64) {
    let section = |id: &str, section_type, number, bar: u64| SectionMarker {
        id: id.to_owned(),
        section_type,
        number,
        start: bar * bars * BAR,
    };
    let package = SongPackage {
        format_version: 1,
        title: "Probe".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: None,
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: vec![Stem {
            id: "keys".to_owned(),
            file: "stems/keys.wav".to_owned(),
            part: "keys".to_owned(),
            role: StemRole::Track,
        }],
        sections: vec![
            section("intro", SectionType::Intro, None, 0),
            section("verse1", SectionType::Verse, Some(1), 1),
            section("verse2", SectionType::Verse, Some(2), 2),
            section("chorus", SectionType::Chorus, None, 3),
            section("outro", SectionType::Outro, None, 4),
        ],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: order.iter().map(|s| (*s).to_owned()).collect(),
            infinite_loop: Vec::new(),
        }],
    };
    fs::create_dir_all(dir.join("stems")).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(dir.join("stems/keys.wav"), spec).unwrap();
    for frame in 0..5 * bars * BAR {
        writer.write_sample((frame % 200) as i16 * 50 + 1).unwrap();
    }
    writer.finalize().unwrap();
}

/// Holds back every block that ends past `from` until the flag is set.
fn gate_from(from: u64) -> (library_gate::Gate, Arc<AtomicBool>) {
    let open = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&open);
    let gate: library_gate::Gate = Arc::new(move |_: &str, block: std::ops::Range<u64>| {
        block.end <= from || flag.load(Ordering::SeqCst)
    });
    (gate, open)
}

mod library_gate {
    pub type Gate = std::sync::Arc<dyn Fn(&str, std::ops::Range<u64>) -> bool + Send + Sync>;
}

struct Shell {
    session: Session,
    song: SongInfo,
    fake: FakeOutput,
    snapshot: Snapshot,
}

impl Shell {
    fn open(dir: &Path, options: SessionOptions) -> Self {
        let fake = FakeOutput::new();
        let mut session = Session::open(dir, Box::new(fake.clone()), options).unwrap();
        let song = session.song().clone();
        let snapshot = session.poll();
        Self {
            session,
            song,
            fake,
            snapshot,
        }
    }

    /// The screen's `send`: errors only show as the last error.
    fn act(&mut self, action: UiAction) {
        if let Some(command) = command_for(action, &self.snapshot, &self.song) {
            let _ = self.session.send(command);
        }
        self.snapshot = self.session.poll();
    }

    fn step(&mut self) {
        let monitor = self.session.disk_monitor();
        assert!(monitor.wait_idle(Duration::from_secs(10)));
        self.fake.render(FRAMES).expect("a stream plays");
        self.snapshot = self.session.poll();
    }

    fn play(&mut self) {
        self.act(UiAction::TogglePlay);
        for _ in 0..200 {
            self.step();
            if self.snapshot.transport == TransportState::Playing {
                return;
            }
        }
        panic!("never started");
    }

    /// Steps until `done` holds, at most `max` callbacks.
    fn until(&mut self, max: usize, done: impl Fn(&Snapshot) -> bool) {
        for _ in 0..max {
            if done(&self.snapshot) {
                return;
            }
            self.step();
        }
        assert!(
            done(&self.snapshot),
            "condition never held: {:?}",
            self.snapshot
        );
    }

    fn view(&self) -> PerformView {
        PerformView::new(&self.snapshot, &self.song)
    }
}

const ALL: &[&str] = &["intro", "verse1", "verse2", "chorus", "verse1", "outro"];

fn marks(view: &PerformView) -> Vec<(bool, bool, bool)> {
    view.pads
        .iter()
        .map(|p| (p.is_now, p.is_next, p.is_queued))
        .collect()
}

#[test]
fn infinite_at_the_last_section_repeats_it_and_the_screen_says_so() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let mut shell = Shell::open(dir.path(), SessionOptions::default());
    shell.play();
    shell.act(UiAction::QueueSection(5));
    shell.until(2_000, |s| s.position == 5);
    shell.act(UiAction::ToggleInfinite);
    shell.step();
    let view = shell.view();
    assert!(view.infinite);
    assert_eq!(view.next.heading, "Next");
    assert_eq!(view.next.name, "Outro");
    assert!(
        view.next.detail.ends_with("· infinite loop"),
        "{}",
        view.next.detail
    );
    assert_eq!(marks(&view)[5], (true, true, false));
    // Past the section end, the outro plays again, the playhead goes back.
    let mut previous = view.map.playhead;
    let mut wrapped = false;
    for _ in 0..(BAR as usize / FRAMES + 4) {
        shell.step();
        let view = shell.view();
        assert_eq!(shell.snapshot.position, 5);
        assert!(view.map.playhead >= 5.0 / 6.0 - 1e-6);
        assert_eq!(view.next.name, "Outro");
        wrapped |= view.map.playhead < previous;
        previous = view.map.playhead;
    }
    assert!(wrapped, "the playhead went back to the start of the outro");
    assert_eq!(shell.snapshot.transport, TransportState::Playing);
    // Infinite off: the song ends after it.
    shell.act(UiAction::ToggleInfinite);
    shell.step();
    let view = shell.view();
    assert_eq!(view.next.name, "End of song");
    assert!(view.pads.iter().all(|p| !p.is_next));
    shell.until(2_000, |s| s.transport == TransportState::Stopped);
    let view = shell.view();
    assert_eq!(view.transport.state, "Stopped");
    assert!(view.pads[0].is_now);
    assert!(!view.infinite);
}

#[test]
fn a_jump_queued_in_the_last_section_executes_at_the_end_of_the_song() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let mut shell = Shell::open(dir.path(), SessionOptions::default());
    shell.play();
    shell.act(UiAction::QueueSection(5));
    shell.until(2_000, |s| s.position == 5);
    shell.step();
    for timing in [JumpTiming::SectionEnd, JumpTiming::NextBar] {
        shell.act(UiAction::SetJumpTiming(timing));
        shell.act(UiAction::QueueSection(1));
        shell.step();
        let view = shell.view();
        assert_eq!(view.next.heading, "Jump queued");
        assert_eq!(view.next.name, "Verse 1");
        assert_eq!(marks(&view)[1], (false, true, true));
        assert_eq!(
            marks(&view)[4],
            (false, false, false),
            "the other Verse 1 is not queued"
        );
        let marker = view.map.jump.clone().unwrap();
        // One-bar sections: section end and next bar are both the song end.
        assert!((marker.at - 1.0).abs() < 1e-4, "{}", marker.at);
        let left = shell.snapshot.queued.as_ref().unwrap().frames_until;
        let source_left = 6 * BAR - (5 * BAR + (shell.snapshot.source_frame - 4 * BAR));
        assert_eq!(left, source_left);
        // Cancel and queue again for the second timing.
        shell.act(UiAction::QueueSection(1));
        assert!(
            shell.snapshot.queued.is_none() || {
                shell.step();
                shell.snapshot.queued.is_none()
            }
        );
    }
    shell.act(UiAction::QueueSection(1));
    shell.until(2_000, |s| s.position != 5);
    assert_eq!(shell.snapshot.position, 1);
    assert_eq!(shell.snapshot.transport, TransportState::Playing);
    let view = shell.view();
    assert_eq!(marks(&view)[1], (true, false, false));
    assert_eq!(marks(&view)[2], (false, true, false));
    assert_eq!(view.map.jump, None);
}

#[test]
fn a_waiting_jump_shows_on_the_map_the_next_card_and_the_status() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let (gate, open) = gate_from(4 * BAR);
    let mut shell = Shell::open(
        dir.path(),
        SessionOptions {
            disk_gate: Some(gate),
            ..SessionOptions::default()
        },
    );
    shell.play();
    shell.act(UiAction::QueueSection(5));
    shell.until(2_000, |s| s.position == 1);
    shell.step();
    let queued = shell.snapshot.queued.clone().expect("still queued");
    assert_eq!((queued.target, queued.deferrals), (5, 1));
    let view = shell.view();
    assert_eq!(view.next.heading, "Jump queued");
    assert_eq!(view.next.name, "Outro");
    assert!(
        view.next
            .detail
            .contains("moved on 1 boundary waiting for its audio"),
        "{}",
        view.next.detail
    );
    let marker = view.map.jump.clone().unwrap();
    // It executes at the end of the verse now, 2/6 of the arrangement.
    assert!((marker.at - 2.0 / 6.0).abs() < 1e-4, "{}", marker.at);
    assert!(
        view.status
            .last_error
            .as_deref()
            .unwrap()
            .contains("waiting for its audio"),
        "{:?}",
        view.status.last_error
    );
    // Its audio arrives; it executes at the next boundary and the error clears.
    open.store(true, Ordering::SeqCst);
    shell.until(2_000, |s| s.position == 5);
    let view = shell.view();
    assert_eq!(view.map.jump, None);
    assert_eq!(view.status.last_error, None);
    assert_eq!(view.next.name, "End of song");
}

#[test]
fn device_lost_then_reopen_refused_then_back() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let mut shell = Shell::open(
        dir.path(),
        SessionOptions {
            reconnect_interval: Duration::from_millis(1),
            ..SessionOptions::default()
        },
    );
    shell.play();
    shell.fake.unplug(FAKE_DEVICE_ID);
    shell.snapshot = shell.session.poll();
    let view = shell.view();
    assert_eq!(
        view.status.device_problem.as_deref(),
        Some("No device: \"Fake Output\" (the device was unplugged)")
    );
    assert_eq!(view.transport.state, "Paused");
    assert_eq!(view.transport.play_label, "Play");
    let lost = view.status.last_error.clone().unwrap();
    assert!(lost.contains("was unplugged"), "{lost}");
    // Play while it is gone: refused, the screen keeps the reason.
    shell.act(UiAction::TogglePlay);
    assert!(matches!(
        shell.snapshot.last_error,
        Some(RuntimeError::DeviceLost { .. })
    ));
    assert_eq!(shell.view().transport.state, "Paused");
    // Back at the wrong rate: the real reason.
    shell.fake.set_rates(FAKE_DEVICE_ID, Some(&[44_100]));
    shell.fake.plug(FAKE_DEVICE_ID);
    std::thread::sleep(Duration::from_millis(5));
    shell.snapshot = shell.session.poll();
    let view = shell.view();
    let problem = view.status.device_problem.clone().unwrap_or_default();
    assert!(
        problem.starts_with("No device: \"Fake Output\" (cannot open it again: "),
        "{problem}"
    );
    assert!(problem.contains("8000 Hz"), "{problem}");
    assert!(
        view.status
            .last_error
            .as_deref()
            .unwrap()
            .contains("cannot be opened again"),
        "{:?}",
        view.status.last_error
    );
    // Back at any rate: connected, error gone, still paused.
    shell.fake.set_rates(FAKE_DEVICE_ID, None);
    std::thread::sleep(Duration::from_millis(5));
    shell.snapshot = shell.session.poll();
    let view = shell.view();
    assert_eq!(view.status.device_problem, None);
    assert_eq!(view.status.last_error, None);
    assert_eq!(view.transport.state, "Paused");
    shell.play();
    assert_eq!(shell.view().transport.state, "Playing");
}

#[test]
fn a_decode_failure_shows_as_the_last_problem() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let (gate, open) = gate_from(BAR);
    let mut shell = Shell::open(
        dir.path(),
        SessionOptions {
            disk_gate: Some(gate),
            ..SessionOptions::default()
        },
    );
    // Everything from verse 2 on is gone after the package was checked.
    let file = OpenOptions::new()
        .write(true)
        .open(dir.path().join("stems/keys.wav"))
        .unwrap();
    file.set_len(44 + 2 * 2 * BAR).unwrap();
    open.store(true, Ordering::SeqCst);
    shell.play();
    shell.until(4_000, |s| s.decode_errors > 0);
    shell.step();
    let view = shell.view();
    let error = view.status.last_error.clone().expect("a problem shows");
    assert!(error.contains("could not decode"), "{error}");
    // The device stays fine: the problem is in the last-problem line.
    assert_eq!(view.status.device_problem, None);
    // A jump waiting and cancelled falls back to the decode failure.
    shell.act(UiAction::QueueSection(0));
    shell.act(UiAction::CancelJump);
    shell.step();
    assert!(
        shell
            .view()
            .status
            .last_error
            .unwrap()
            .contains("could not decode")
    );
}

#[test]
fn a_song_with_one_section() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), &["chorus"]);
    let mut shell = Shell::open(dir.path(), SessionOptions::default());
    let view = shell.view();
    assert_eq!(view.map.blocks.len(), 1);
    assert!((view.map.blocks[0].width - 1.0).abs() < 1e-6);
    assert_eq!(view.next.name, "End of song");
    assert_eq!(marks(&view), [(true, false, false)]);
    shell.play();
    // Its own pad queues a jump to itself: it plays again.
    shell.act(UiAction::QueueSection(0));
    shell.step();
    let view = shell.view();
    assert_eq!(marks(&view), [(true, true, true)]);
    assert_eq!(view.next.heading, "Jump queued");
    assert_eq!(view.next.name, "Chorus");
    let at = view.map.jump.unwrap().at;
    assert!((at - 1.0).abs() < 1e-4, "{at}");
    for _ in 0..(BAR as usize / FRAMES + 4) {
        shell.step();
    }
    assert_eq!(shell.snapshot.transport, TransportState::Playing);
    assert!(shell.snapshot.queued.is_none());
    // Loop section queues a loop; pressed again, it drops it.
    shell.act(UiAction::ToggleLiveLoop);
    shell.step();
    assert!(shell.view().live_loop);
    assert_eq!(shell.view().next.heading, "Loop queued");
    assert_eq!(shell.snapshot.queued.as_ref().unwrap().kind, JumpKind::Loop);
    shell.act(UiAction::ToggleLiveLoop);
    shell.step();
    assert!(!shell.view().live_loop);
    // Keys past the arrangement do nothing.
    assert_eq!(
        command_for(UiAction::QueueSection(1), &shell.snapshot, &shell.song),
        None
    );
}

#[test]
fn labels_that_share_a_first_word_and_a_repeated_section() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let mut shell = Shell::open(dir.path(), SessionOptions::default());
    let view = shell.view();
    let labels: Vec<&str> = view.pads.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Intro", "Verse 1", "Verse 2", "Chorus", "Verse 1", "Outro"]
    );
    let shorts: Vec<&str> = view.map.blocks.iter().map(|b| b.short.as_str()).collect();
    assert_eq!(shorts, ["In", "V1", "V2", "C", "V1", "Out"]);
    assert_eq!(view.pads[1].kind, SectionKind::Verse);
    assert_eq!(view.pads[2].kind, SectionKind::Verse);
    // In the second Verse 1, only that pad is Now, and the map places the
    // playhead in the fifth block.
    shell.play();
    shell.act(UiAction::QueueSection(4));
    shell.step();
    let view = shell.view();
    assert_eq!(marks(&view)[4], (false, true, true));
    assert_eq!(marks(&view)[1], (false, false, false));
    shell.until(2_000, |s| s.position == 4);
    shell.step();
    let view = shell.view();
    assert_eq!(marks(&view)[4], (true, false, false));
    assert_eq!(marks(&view)[1], (false, false, false));
    assert!(view.map.blocks[4].is_now && !view.map.blocks[1].is_now);
    assert!(view.map.playhead > 4.0 / 6.0 && view.map.playhead < 5.0 / 6.0);
    assert_eq!(view.now.name, "Verse 1");
    assert_eq!(view.next.name, "Outro");
    // Queue the first Verse 1 from the second: the target is position 1.
    shell.act(UiAction::QueueSection(1));
    shell.step();
    assert_eq!(shell.snapshot.queued.as_ref().unwrap().target, 1);
    let view = shell.view();
    assert!(view.map.blocks[1].is_queued && !view.map.blocks[4].is_queued);
}

#[test]
fn stop_while_disconnected_reaches_stopped_without_audio_callbacks() {
    // What `Perform::close_song` waits for: Stop, then poll until Stopped.
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), ALL);
    let mut shell = Shell::open(dir.path(), SessionOptions::default());
    shell.play();
    shell.fake.unplug(FAKE_DEVICE_ID);
    shell.snapshot = shell.session.poll();
    assert!(matches!(
        shell.snapshot.device,
        DeviceStatus::Disconnected { .. }
    ));
    shell.session.send(app::Command::Stop).unwrap();
    assert_eq!(shell.session.poll().transport, TransportState::Stopped);
}

#[test]
fn next_bar_marker_inside_a_long_section_and_its_time() {
    let dir = tempfile::tempdir().unwrap();
    write_song_bars(dir.path(), ALL, 4);
    let mut shell = Shell::open(dir.path(), SessionOptions::default());
    shell.play();
    // 1.3 bars into the intro.
    shell.until(2_000, |s| s.source_frame >= BAR + BAR / 3);
    shell.act(UiAction::ToggleJumpTiming);
    shell.act(UiAction::QueueSection(3));
    shell.step();
    let s = shell.snapshot.clone();
    assert_eq!(s.jump_timing, JumpTiming::NextBar);
    let q = s.queued.clone().unwrap();
    assert_eq!(
        q.frames_until,
        2 * BAR - s.source_frame,
        "the next bar line"
    );
    let view = shell.view();
    let marker = view.map.jump.clone().unwrap();
    let total = (6 * 4 * BAR) as f64;
    assert!(
        (f64::from(marker.at) - (2 * BAR) as f64 / total).abs() < 1e-5,
        "{}",
        marker.at
    );
    let secs = q.frames_until as f64 / f64::from(RATE);
    assert_eq!(marker.label, format!("Jump · in {secs:.1} s"));
    assert_eq!(view.next.detail, format!("Executes in {secs:.1} s"));
    shell.until(2_000, |s| s.position == 3);
    assert_eq!(
        shell.snapshot.source_frame - 3 * 4 * BAR,
        shell.snapshot.output_frame - 2 * BAR
    );
    // Section end: the marker moves to the end of the chorus.
    shell.act(UiAction::SetJumpTiming(JumpTiming::SectionEnd));
    shell.act(UiAction::QueueSection(0));
    shell.step();
    let view = shell.view();
    let at = f64::from(view.map.jump.unwrap().at);
    assert!((at - 4.0 / 6.0).abs() < 1e-5, "{at}");
}
