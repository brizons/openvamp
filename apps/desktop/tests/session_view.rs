//! S6 end to end without a window or hardware: a real `Session` on the
//! app's `FakeOutput`, driven through the view-model's actions, drawn as a
//! `PerformView`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use app::testing::FakeOutput;
use app::{Session, SessionOptions, Snapshot, SongInfo, TransportState};
use domain::{
    Arrangement, SectionMarker, SectionType, SongPackage, Stem, StemRole, TempoPoint,
    TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use openvamp_desktop::action::{UiAction, command_for};
use openvamp_desktop::view::PerformView;

const RATE: u32 = 8_000;
/// 120 bpm in 4/4: a bar is two seconds.
const BAR: u64 = 2 * RATE as u64;
const FRAMES: usize = 256;

/// Intro, verse 1, chorus and outro, one bar each, one mono track stem.
fn write_song(dir: &Path) {
    let section = |id: &str, section_type, number, bar: u64| SectionMarker {
        id: id.to_owned(),
        section_type,
        number,
        start: bar * BAR,
    };
    let package = SongPackage {
        format_version: 1,
        title: "Shell Test".to_owned(),
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
            section("verse", SectionType::Verse, Some(1), 1),
            section("chorus", SectionType::Chorus, None, 2),
            section("outro", SectionType::Outro, None, 3),
        ],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: ["intro", "verse", "chorus", "outro"]
                .map(str::to_owned)
                .to_vec(),
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
    for frame in 0..4 * BAR {
        writer.write_sample((frame % 200) as i16 * 50).unwrap();
    }
    writer.finalize().unwrap();
}

struct Shell {
    session: Session,
    song: SongInfo,
    fake: FakeOutput,
    snapshot: Snapshot,
}

impl Shell {
    fn open(dir: &Path) -> Self {
        let fake = FakeOutput::new();
        let mut session =
            Session::open(dir, Box::new(fake.clone()), SessionOptions::default()).unwrap();
        let song = session.song().clone();
        let snapshot = session.poll();
        Self {
            session,
            song,
            fake,
            snapshot,
        }
    }

    /// What the screen does when `action` is clicked or keyed.
    fn act(&mut self, action: UiAction) {
        if let Some(command) = command_for(action, &self.snapshot, &self.song) {
            self.session.send(command).unwrap();
        }
        self.snapshot = self.session.poll();
    }

    /// One audio callback, then one turn of the screen's timer.
    fn step(&mut self) {
        let monitor = self.session.disk_monitor();
        assert!(monitor.wait_idle(Duration::from_secs(10)));
        self.fake.render(FRAMES).expect("a stream plays");
        self.snapshot = self.session.poll();
    }

    fn view(&self) -> PerformView {
        PerformView::new(&self.snapshot, &self.song)
    }
}

#[test]
fn the_perform_screen_follows_a_real_session() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path());
    let mut shell = Shell::open(dir.path());

    let view = shell.view();
    let labels: Vec<&str> = view.pads.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["Intro", "Verse 1", "Chorus", "Outro"]);
    assert_eq!(view.transport.state, "Stopped");
    // The device plays: the header's chip names it, and the notes say
    // nothing of it.
    assert_eq!(view.device.name, "Fake Output");
    assert!(
        view.device.meta.ends_with(" · 256 · 32.0 ms"),
        "{}",
        view.device.meta
    );
    assert_eq!(view.status.device_problem, None);

    // Play waits for the disk, then the timer's polls start it.
    shell.act(UiAction::TogglePlay);
    for _ in 0..100 {
        shell.step();
        if shell.snapshot.transport == TransportState::Playing {
            break;
        }
    }
    let view = shell.view();
    assert_eq!(view.transport.state, "Playing");
    assert_eq!(view.transport.play_label, "Pause");
    assert!(view.pads[0].is_now && view.pads[1].is_next);

    // A pad queues a jump; the map and the Next card show it once the
    // audio thread has taken the command.
    shell.act(UiAction::QueueSection(2));
    shell.step();
    let view = shell.view();
    assert!(view.pads[2].is_next && view.pads[2].is_queued);
    assert_eq!(view.next.heading, "Jump queued");
    assert_eq!(view.next.name, "Chorus");
    let marker = view.map.jump.expect("the queued jump is on the map");
    assert!(marker.label.starts_with("Jump · in "), "{}", marker.label);
    assert!(marker.at > view.map.playhead && marker.at <= 0.25);

    // The same pad again cancels it.
    shell.act(UiAction::QueueSection(2));
    shell.step();
    assert_eq!(shell.view().map.jump, None);

    // Queued again while the intro still plays, it executes at the end of
    // the intro: one bar, so within two bars of callbacks.
    assert_eq!(shell.snapshot.position, 0, "the intro is still playing");
    shell.act(UiAction::QueueSection(2));
    let callbacks = 2 * BAR as usize / FRAMES;
    for _ in 0..callbacks {
        if shell.snapshot.position != 0 {
            break;
        }
        shell.step();
    }
    assert_ne!(
        shell.snapshot.position, 0,
        "the jump did not execute within {callbacks} callbacks: {:?}",
        shell.snapshot
    );
    let view = shell.view();
    assert!(view.pads[2].is_now && view.pads[3].is_next);
    assert_eq!(view.now.name, "Chorus");
    assert_eq!(view.map.jump, None);
    assert_eq!(view.status.last_error, None);
}
