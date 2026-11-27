//! S8 sequences from the independent verification of T6, on the pure units
//! and a real `Session` with `FakeOutput`. The swap probes go through
//! `song::replace`, which prepares the pick and checks the device, closes
//! the current song, and only then opens the device; two of them changed
//! with it (see each).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use app::testing::FakeOutput;
use app::{AudioOutput, Command, OpenError, Session, SessionOptions, TransportState};
use domain::{
    Arrangement, SectionMarker, SectionType, SongPackage, Stem, StemRole, TempoPoint,
    TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use openvamp_desktop::ending::{Dialog, EndRequest, Ending, end_request};
use openvamp_desktop::hold::{PANIC_HOLD, PanicHold};
use openvamp_desktop::keys::{HeldKeys, KeyAction, KeyPlatform, KeyPress, Mods};
use openvamp_desktop::song::{Closing, Replaced, Song, replace, stop_and_wait};

const FRAMES: usize = 256;

fn write_song(dir: &Path, title: &str, rate: u32) {
    let bar = 2 * u64::from(rate);
    let section = |id: &str, section_type, at: u64| SectionMarker {
        id: id.to_owned(),
        section_type,
        number: None,
        start: at * bar,
    };
    let package = SongPackage {
        format_version: 1,
        title: title.to_owned(),
        artist: None,
        sample_rate: rate,
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
            section("intro", SectionType::Intro, 0),
            section("verse", SectionType::Verse, 1),
        ],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["intro".to_owned(), "verse".to_owned()],
            infinite_loop: Vec::new(),
        }],
    };
    fs::create_dir_all(dir.join("stems")).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(dir.join("stems/keys.wav"), spec).unwrap();
    for frame in 0..2 * bar {
        writer
            .write_sample(2_000 + (frame % 200) as i16 * 50)
            .unwrap();
    }
    writer.finalize().unwrap();
}

struct Playing {
    session: Session,
    fake: FakeOutput,
}

impl Playing {
    fn open_on(dir: &Path, fake: FakeOutput, options: SessionOptions) -> Result<Self, OpenError> {
        let session = Session::open(dir, Box::new(fake.clone()), options)?;
        Ok(Self { session, fake })
    }

    fn open(dir: &Path) -> Result<Self, OpenError> {
        Self::open_on(dir, FakeOutput::new(), SessionOptions::default())
    }

    fn step(&mut self) -> Vec<f32> {
        assert!(
            self.session
                .disk_monitor()
                .wait_idle(Duration::from_secs(10))
        );
        self.fake.render(FRAMES).expect("a stream plays")
    }

    fn play(&mut self) {
        self.session.send(Command::Play).unwrap();
        for _ in 0..200 {
            self.step();
            if self.session.poll().transport == TransportState::Playing {
                return;
            }
        }
        panic!("the song never started");
    }
}

fn song_on(dir: &Path, fake: &FakeOutput) -> Song {
    Song::open(dir, &SessionOptions::default(), Box::new(fake.clone())).unwrap()
}

fn play(song: &mut Song, fake: &FakeOutput) {
    song.session.send(Command::Play).unwrap();
    for _ in 0..200 {
        assert!(
            song.session
                .disk_monitor()
                .wait_idle(Duration::from_secs(10))
        );
        fake.render(FRAMES).unwrap();
        song.poll();
        if song.snapshot.transport == TransportState::Playing {
            return;
        }
    }
    panic!("the song never started");
}

/// `replace` with every output handed out from `device`.
fn replace_on(
    current: &mut Option<Song>,
    dir: &Path,
    device: &FakeOutput,
    sleep: impl FnMut(Duration),
) -> Result<Replaced, OpenError> {
    let outputs = || Box::new(device.clone()) as Box<dyn AudioOutput>;
    replace(current, dir, &SessionOptions::default(), outputs, sleep)
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |p, s| p.max(s.abs()))
}

fn ramped_down(heard: &[Vec<f32>]) -> bool {
    let Some(last) = heard.last() else {
        return false;
    };
    let (head, _) = last.split_at(last.len() / 4);
    peak(head) > 0.01 && last.last() == Some(&0.0)
}

// --- Opening in place -----------------------------------------------------

#[test]
fn swap_while_a_jump_is_queued_stops_the_old_song_with_its_ramp() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_song(first.path(), "First", 8_000);
    write_song(second.path(), "Second", 8_000);
    let old_fake = FakeOutput::new();
    let mut current = Some(song_on(first.path(), &old_fake));
    let old = current.as_mut().unwrap();
    play(old, &old_fake);
    old.session
        .send(Command::QueueJump { position: 1 })
        .unwrap();
    old_fake.render(FRAMES).unwrap();
    assert!(old.session.poll().queued.is_some(), "the jump is queued");

    let new_fake = FakeOutput::new();
    let mut heard = Vec::new();
    let replaced = replace_on(&mut current, second.path(), &new_fake, |_| {
        heard.push(old_fake.render(FRAMES).unwrap());
    })
    .unwrap();
    let previous = replaced.previous.expect("the old song closed");
    assert_eq!(previous.closing, Closing::Stopped { refused: None });
    assert!(ramped_down(&heard), "a ramp, not a cut");

    let new = current.as_mut().unwrap();
    let snapshot = new.session.poll();
    assert_eq!(new.session.song().title, "Second");
    assert_eq!(snapshot.transport, TransportState::Stopped);
    assert!(
        snapshot.queued.is_none(),
        "the old jump does not carry over"
    );
    assert!(snapshot.last_error.is_none(), "{:?}", snapshot.last_error);
}

/// Changed with the swap: `replace` drops the old session itself, so the
/// probe checks that its device is closed and that nothing played while it
/// stopped, instead of rendering it afterwards.
#[test]
fn swap_while_the_old_song_is_starting_cancels_its_waiting_play() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_song(first.path(), "First", 8_000);
    write_song(second.path(), "Second", 8_000);
    // The old song's disk holds every block back, so play waits (Starting).
    let open_gate = Arc::new(AtomicBool::new(false));
    let gate_flag = Arc::clone(&open_gate);
    let options = SessionOptions {
        disk_gate: Some(Arc::new(move |_: &str, _| gate_flag.load(Ordering::SeqCst))),
        ..SessionOptions::default()
    };
    let old_fake = FakeOutput::new();
    let mut old = Song::open(first.path(), &options, Box::new(old_fake.clone())).unwrap();
    old.session.send(Command::Play).unwrap();
    old_fake.render(FRAMES).unwrap();
    assert_eq!(old.session.poll().transport, TransportState::Starting);
    let mut current = Some(old);

    let new_fake = FakeOutput::new();
    let mut tries = 0;
    let mut heard = Vec::new();
    let started = Instant::now();
    let replaced = replace_on(&mut current, second.path(), &new_fake, |_| {
        tries += 1;
        // The disk catches up while closing: the waiting play must not start.
        open_gate.store(true, Ordering::SeqCst);
        heard.push(old_fake.render(FRAMES).unwrap());
    })
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the old session drops promptly"
    );
    assert_eq!(
        replaced.previous.unwrap().closing,
        Closing::Stopped { refused: None }
    );
    assert_eq!(tries, 0, "Stop ends a waiting play at once, with no wait");
    assert!(
        heard.iter().all(|audio| peak(audio) == 0.0),
        "the waiting play never starts"
    );
    assert!(!old_fake.is_streaming(), "the old device is closed");
    assert_eq!(current.unwrap().session.song().title, "Second");
}

/// The device is checked before the current song stops
/// ([`app::AudioOutput::check`]), so a pick it cannot play leaves the
/// current song playing and opens nothing.
#[test]
fn a_pick_at_a_rate_the_device_refuses_leaves_the_current_song_playing() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_song(first.path(), "First", 48_000);
    write_song(second.path(), "Second", 44_100);
    let old_fake = FakeOutput::new();
    let mut current = Some(song_on(first.path(), &old_fake));
    play(current.as_mut().unwrap(), &old_fake);

    // The app gives every open its own output; this one only runs at 48 kHz.
    let refusing = FakeOutput::without_devices().with_device("Only 48k", 2, Some(&[48_000]));
    let refused = replace_on(&mut current, second.path(), &refusing, |_| {
        panic!("the current song does not stop")
    });
    let Err(error) = refused else {
        panic!("a 44.1 kHz song does not open on a 48 kHz-only device");
    };
    assert!(format!("{error}").contains("44100"), "{error}");
    assert_eq!(refusing.open_attempts(), 0, "no device was opened");
    let old = current.as_mut().unwrap();
    assert_eq!(old.session.song().title, "First");
    let heard = old_fake.render(FRAMES).unwrap();
    assert_eq!(old.session.poll().transport, TransportState::Playing);
    assert!(peak(&heard) > 0.01, "the current song is still heard");
    assert!(old.session.poll().last_error.is_none());
}

/// Changed with the swap (it used to record that no pick opened while a
/// song was loaded): on a device that allows one stream at a time, the
/// current song now closes before the pick opens the device.
#[test]
fn on_a_one_stream_device_a_pick_opens_once_the_current_song_closed() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_song(first.path(), "First", 8_000);
    write_song(second.path(), "Second", 8_000);
    let shared = FakeOutput::new();
    let mut current = Some(song_on(first.path(), &shared));
    play(current.as_mut().unwrap(), &shared);

    let replaced = replace_on(&mut current, second.path(), &shared, |_| {
        shared.render(FRAMES);
    })
    .unwrap();
    assert!(replaced.start_error.is_none(), "{:?}", replaced.start_error);
    assert_eq!(current.as_ref().unwrap().session.song().title, "Second");
    assert!(shared.is_streaming());
}

#[test]
fn a_stop_refused_for_the_whole_window_then_the_swap_reports_it() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_song(first.path(), "First", 8_000);
    write_song(second.path(), "Second", 8_000);
    let old_fake = FakeOutput::new();
    let mut current = Some(song_on(first.path(), &old_fake));
    let old = current.as_mut().unwrap();
    play(old, &old_fake);
    // Fill the queue: the audio thread takes nothing for the whole window.
    let mut sent = 0;
    while old
        .session
        .send(Command::SetJumpTiming(app::JumpTiming::NextBar))
        .is_ok()
    {
        sent += 1;
        assert!(sent < 10_000);
    }

    let new_fake = FakeOutput::new();
    let replaced = replace_on(&mut current, second.path(), &new_fake, |_| {}).unwrap();
    let previous = replaced.previous.as_ref().unwrap();
    assert!(
        matches!(previous.closing, Closing::Refused(_)),
        "{previous:?}"
    );
    let problems = replaced.problems();
    assert!(problems[0].contains("without its ramp"), "{problems:?}");

    // The new song is unaffected and plays.
    let new = current.as_mut().unwrap();
    play(new, &new_fake);
    assert!(peak(&new_fake.render(FRAMES).unwrap()) > 0.01);
    assert!(new.session.poll().last_error.is_none());
}

#[test]
fn closing_during_panic_waits_for_the_panic_fade_in_real_time_terms() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), "First", 48_000);
    let mut playing = Playing::open(dir.path()).unwrap();
    playing.play();
    playing.session.send(Command::Panic).unwrap();
    // 5 ms of audio per 5 ms wait, as a real device at 48 kHz renders it.
    let fake = playing.fake.clone();
    let mut heard = Vec::new();
    let closing = stop_and_wait(&mut playing.session, |_| {
        heard.push(fake.render(240).unwrap());
    });
    eprintln!(
        "close during panic: {closing:?} after {} callbacks",
        heard.len()
    );
    let last = heard.last().unwrap();
    eprintln!("peak of the last callback heard: {}", peak(last));
    assert!(
        peak(last) < 0.05,
        "the panic fade is nearly done whatever closing reports"
    );
}

#[test]
fn closing_a_song_whose_device_was_unplugged_does_not_wait_the_whole_window() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), "First", 8_000);
    let mut playing = Playing::open(dir.path()).unwrap();
    playing.play();
    playing.fake.unplug(app::testing::FAKE_DEVICE_ID);
    let mut tries = 0;
    let closing = stop_and_wait(&mut playing.session, |_| tries += 1);
    eprintln!("close after unplug: {closing:?} after {tries} waits");
    assert_eq!(closing, Closing::Stopped { refused: None });
    assert!(tries <= 1, "{tries}");
}

// --- Ending -----------------------------------------------------------------

#[test]
fn end_requests_while_a_ramp_runs_or_the_question_is_open() {
    use TransportState::*;
    for transport in [Stopping, Panicking, Pausing, Paused, Stopped] {
        assert_eq!(
            end_request(Some(transport), false, false),
            EndRequest::EndNow,
            "{transport:?}"
        );
        assert_eq!(
            end_request(Some(transport), false, true),
            EndRequest::Wait,
            "{transport:?}"
        );
    }
    for transport in [Playing, Starting] {
        assert_eq!(end_request(Some(transport), false, false), EndRequest::Ask);
        assert_eq!(end_request(Some(transport), false, true), EndRequest::Wait);
    }
    // The question open when the song went away (no song): still waits.
    assert_eq!(end_request(None, false, true), EndRequest::Wait);
    assert_eq!(end_request(None, false, false), EndRequest::EndNow);
    // Changed with the per-platform order: the ending answer is the first
    // one in the Mac's dialog.
    let question = Ending::Quit.question(Dialog::Mac);
    assert!(question.ends(Some(0)));
    assert!(!question.ends(Some(1)) && !question.ends(None) && !question.ends(Some(2)));
}

// --- Hold -----------------------------------------------------------------

#[test]
fn a_hold_cancelled_then_released_then_pressed_again_counts_afresh() {
    let t0 = Instant::now();
    let mut hold = PanicHold::new();
    hold.press(t0);
    assert!(!hold.tick(t0 + PANIC_HOLD / 2));
    hold.cancel();
    assert!(
        !hold.tick(t0 + PANIC_HOLD * 2),
        "a cancelled hold never fires"
    );
    assert!(
        !hold.release(t0 + PANIC_HOLD * 2),
        "its late release fires nothing"
    );
    assert_eq!(hold.fill(t0 + PANIC_HOLD * 2), 0.0);
    let t1 = t0 + PANIC_HOLD * 3;
    hold.press(t1);
    assert!(
        !hold.tick(t1 + PANIC_HOLD / 2),
        "the new hold counts from its own press"
    );
    assert!(hold.tick(t1 + PANIC_HOLD), "and fires after a full hold");
    assert!(!hold.release(t1 + PANIC_HOLD * 2), "once");
    // A cancel after firing, while still held, then the release: nothing more.
    hold.press(t1 + PANIC_HOLD * 3);
    assert!(hold.tick(t1 + PANIC_HOLD * 4));
    hold.cancel();
    assert!(!hold.release(t1 + PANIC_HOLD * 5));
    // A cancel then an X11 repeat of the still-held key starts a new hold.
    hold.press(t1 + PANIC_HOLD * 6);
    hold.cancel();
    hold.press(t1 + PANIC_HOLD * 7);
    assert!(!hold.tick(t1 + PANIC_HOLD * 7 + PANIC_HOLD / 2));
}

// --- Held keys ----------------------------------------------------------------

fn plain(key: &str) -> KeyPress<'_> {
    KeyPress {
        key,
        mods: Mods::default(),
    }
}

fn with_cmd(key: &str) -> KeyPress<'_> {
    KeyPress {
        key,
        mods: Mods {
            platform: true,
            control: !cfg!(target_os = "macos"),
            ..Mods::default()
        },
    }
}

#[test]
fn x11_repeats_of_digits_and_space_mixed_with_shortcuts_act_once() {
    let mut keys = HeldKeys::new();
    assert!(keys.press(&plain("2"), false).is_some());
    // X11 repeat: a fresh press, no release in between.
    assert_eq!(keys.press(&plain("2"), false), None);
    assert!(keys.press(&plain("space"), false).is_some());
    assert_eq!(keys.press(&plain("space"), false), None);
    assert_eq!(keys.press(&plain("2"), false), None, "2 is still down");
    // A shortcut in between does not disturb the tracked keys.
    assert_eq!(keys.press(&with_cmd("1"), false), None);
    assert_eq!(keys.press(&plain("space"), false), None);
    keys.release("space");
    assert!(
        keys.press(&plain("space"), false).is_some(),
        "acts again after its release"
    );
    keys.release("2");
    assert!(keys.press(&plain("2"), false).is_some());
    // Escape and C are one control but two keys.
    assert!(keys.press(&plain("escape"), false).is_some());
    assert!(keys.press(&plain("c"), false).is_some());
    // The panic key is never swallowed: every press reaches the hold.
    assert_eq!(keys.press(&plain("x"), false), Some(KeyAction::PanicDown));
    assert_eq!(keys.press(&plain("x"), false), Some(KeyAction::PanicDown));
    // Upper and lower case are one key.
    assert!(keys.press(&plain("S"), false).is_some());
    assert_eq!(keys.press(&plain("s"), false), None);
    keys.release("S");
    assert!(keys.press(&plain("s"), false).is_some());
}

/// macOS does not deliver the key-up of a key released while Command is
/// held; Command going down counts every key as up, so the next press of a
/// key let go under it acts (this replaces the probe that recorded the
/// swallowed press).
#[test]
fn a_plain_key_released_while_cmd_is_held_acts_on_its_next_press() {
    let mut keys = HeldKeys::on(KeyPlatform::Mac);
    assert!(keys.press(&plain("space"), false).is_some(), "play");
    keys.modifiers_changed(with_cmd("space").mods);
    // Space's key-up while Cmd is held never arrives; Cmd goes up.
    keys.modifiers_changed(Mods::default());
    assert!(
        keys.press(&plain("space"), false).is_some(),
        "the next Space acts"
    );
}
