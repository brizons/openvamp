//! S2 through the `app` API: the routing a session plays with (F8.2-F8.4),
//! the channels it asks the output for, and the routing kept when the device
//! is lost and comes back (F8 acceptance).
//!
//! The band song holds every stem at its own constant level on its own bus,
//! powers of two apart, so each output sample says exactly which buses
//! reached it.

use std::fs;
use std::path::Path;
use std::time::Duration;

use app::testing::FakeOutput;
use app::{
    Command, DeviceStatus, JumpTiming, LossReason, OutputError, RuntimeError, Session,
    SessionOptions, SetlistCommand, SetlistSession, Snapshot, TransportState,
};
use domain::{
    Arrangement, Bus, Destination, Routing, SectionMarker, SectionType, Setlist, SetlistItem,
    SongItem, SongPackage, Stem, StemRole, TempoPoint, TimeSignaturePoint, Transition, Volume,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::Library;

const RATE: u32 = 8_000;
/// Four seconds.
const FRAMES: usize = 4 * RATE as usize;
/// The 20 ms gain ramp at [`RATE`].
const RAMP: usize = 160;

const DRUMS: f32 = 0.5;
const BASS: f32 = 0.25;
const KEYS: f32 = 0.125;
const GUITAR: f32 = 0.0625;
const VOCALS: f32 = 0.031_25;
const GUIDE: f32 = 0.015_625;
const ALL: f32 = DRUMS + BASS + KEYS + GUITAR + VOCALS + GUIDE;

/// Writes the band song into `dir`: one stem per bus at its level, and a
/// silent click stem so the generated click stays off.
fn write_band(dir: &Path) {
    let stems = [
        ("drums", StemRole::Track, DRUMS),
        ("bass", StemRole::Track, BASS),
        ("keys", StemRole::Track, KEYS),
        ("guitar", StemRole::Track, GUITAR),
        ("vocals", StemRole::Track, VOCALS),
        ("guide", StemRole::Guide, GUIDE),
        ("click", StemRole::Click, 0.0),
    ];
    let package = SongPackage {
        format_version: 1,
        title: "Band".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: None,
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: stems
            .iter()
            .map(|&(id, role, _)| Stem {
                id: id.to_owned(),
                file: format!("stems/{id}.wav"),
                part: id.to_owned(),
                role,
            })
            .collect(),
        sections: vec![SectionMarker {
            id: "verse".to_owned(),
            section_type: SectionType::Verse,
            number: None,
            start: 0,
        }],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["verse".to_owned()],
            infinite_loop: Vec::new(),
        }],
    };
    fs::create_dir_all(dir.join("stems")).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    for (id, _, level) in stems {
        let mut writer = WavWriter::create(dir.join(format!("stems/{id}.wav")), spec).unwrap();
        for _ in 0..FRAMES {
            writer.write_sample(level).unwrap();
        }
        writer.finalize().unwrap();
    }
}

fn band() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_band(dir.path());
    dir
}

/// One device with `outputs` outputs that runs at any rate.
fn device(outputs: u16) -> FakeOutput {
    FakeOutput::without_devices().with_device("Interface", outputs, None)
}

fn options(routing: Option<Routing>) -> SessionOptions {
    SessionOptions {
        routing,
        reconnect_interval: Duration::ZERO,
        ..SessionOptions::default()
    }
}

fn open(dir: &Path, fake: &FakeOutput, routing: Option<Routing>) -> Session {
    Session::open(dir, Box::new(fake.clone()), options(routing)).expect("the session opens")
}

/// One callback of 256 frames once the disk has served what it may, then a
/// poll.
fn step(session: &mut Session, fake: &FakeOutput) -> (Vec<f32>, Snapshot) {
    assert!(session.disk_monitor().wait_idle(Duration::from_secs(10)));
    let out = fake.render(256).expect("a stream plays");
    (out, session.poll())
}

/// Plays until the transport runs, then `frames` more frames; returns what
/// those frames played.
fn play(session: &mut Session, fake: &FakeOutput, frames: usize) -> Vec<f32> {
    session.send(Command::Play).unwrap();
    for _ in 0..100 {
        if step(session, fake).1.output_frame > 0 {
            return run(session, fake, frames);
        }
    }
    panic!("the transport never started");
}

/// `frames` more frames, in callbacks of 256.
fn run(session: &mut Session, fake: &FakeOutput, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    for _ in 0..frames.div_ceil(256) {
        out.extend(step(session, fake).0);
    }
    out
}

/// The last frame of `out`, interleaved in `channels` channels.
fn last(out: &[f32], channels: usize) -> Vec<f32> {
    assert_eq!(
        out.len() % channels,
        0,
        "whole frames of {channels} channels"
    );
    out[out.len() - channels..].to_vec()
}

/// The 8-output preset (F8.3) with `bus` sent to `destination`.
fn preset_with(outputs: u16, bus: Bus, destination: Destination) -> Routing {
    let mut routing = Routing::preset(outputs);
    routing.buses.insert(bus, destination);
    routing
}

#[test]
fn the_port_is_asked_for_the_highest_output_the_fitted_routing_uses_and_two_at_least() {
    let oversize = preset_with(
        32,
        Bus::Vocals,
        Destination::Stereo {
            left: 31,
            right: 32,
        },
    );
    let cases = [
        ("stereo on 2", 2, None, 2),
        ("stereo on 8", 8, None, 2),
        ("preset 1", 1, Some(Routing::preset(1)), 1),
        ("preset 2", 2, Some(Routing::preset(2)), 2),
        ("preset 8", 8, Some(Routing::preset(8)), 8),
        // Its highest destination is Vocals on 7/8.
        ("preset 32", 32, Some(Routing::preset(32)), 8),
        // Vocals fall back to 1/2 on 8 outputs: Keys & Pads on 5/6 is the
        // highest left.
        ("oversize on 8", 8, Some(oversize.clone()), 6),
    ];
    for (case, outputs, routing, channels) in cases {
        let dir = band();
        let fake = device(outputs);
        let mut session = open(dir.path(), &fake, routing.clone());
        assert_eq!(fake.opened()[0].channels, channels, "{case}: requested");
        let snapshot = session.poll();
        let DeviceStatus::Connected(info) = &snapshot.device else {
            panic!("{case}: the device plays");
        };
        assert_eq!(info.channels, channels, "{case}: opened");
        let in_use = match &routing {
            Some(routing) => routing.fit(outputs),
            None => Routing::stereo(outputs.min(2)),
        };
        assert_eq!(snapshot.mixer.routing, in_use, "{case}: the routing in use");
        assert_eq!(
            fake.render(4).map(|out| out.len()),
            Some(4 * usize::from(channels)),
            "{case}: frames of {channels} channels reach the device"
        );
    }
    let fitted = oversize.fit(8);
    assert_eq!(fitted.destination(Bus::Vocals), Destination::MAIN);
}

#[test]
fn each_bus_plays_on_its_own_outputs_of_an_eight_output_device() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::preset(8)));
    let out = play(&mut session, &fake, 1_024);
    // Click & Guide 1, Drums 2, Bass 3, Guitars 4, Keys & Pads 5/6, Vocals 7/8.
    assert_eq!(
        last(&out, 8),
        [GUIDE, DRUMS, BASS, GUITAR, KEYS, KEYS, VOCALS, VOCALS]
    );
}

#[test]
fn a_one_output_device_plays_the_mono_fold_of_every_bus() {
    for routing in [None, Some(Routing::preset(2)), Some(Routing::preset(1))] {
        let dir = band();
        let fake = device(1);
        let mut session = open(dir.path(), &fake, routing.clone());
        assert_eq!(fake.opened()[0].channels, 1, "{routing:?}");
        let out = play(&mut session, &fake, 1_024);
        assert_eq!(last(&out, 1), [ALL], "{routing:?}: (left + right) / 2");
    }
}

#[test]
fn a_routing_changed_while_playing_moves_the_buses_over_the_gain_ramp() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::preset(8)));
    let mut out = play(&mut session, &fake, 512);
    // Drums join Bass on output 3.
    session
        .set_routing(preset_with(8, Bus::Drums, Destination::Mono(3)))
        .unwrap();
    out.extend(run(&mut session, &fake, 1_024));
    assert_eq!(fake.open_attempts(), 1, "the stream plays on");
    assert_eq!(
        last(&out, 8),
        [GUIDE, 0.0, BASS + DRUMS, GUITAR, KEYS, KEYS, VOCALS, VOCALS]
    );
    // From the last frame before the change on.
    let frames: Vec<&[f32]> = out[8 * 511..].chunks_exact(8).collect();
    let bound = DRUMS / RAMP as f32 * 1.001;
    for channel in 0..8 {
        for pair in frames.windows(2) {
            let step = (pair[1][channel] - pair[0][channel]).abs();
            assert!(step <= bound, "output {} steps by {step}", channel + 1);
        }
    }
    assert_eq!(
        session.poll().mixer.routing,
        preset_with(8, Bus::Drums, Destination::Mono(3))
    );
}

/// Runs callbacks until the stream has opened `opened` times; returns what
/// the old stream played meanwhile.
fn until_reopened(session: &mut Session, fake: &FakeOutput, opened: usize) -> Vec<f32> {
    let mut old = Vec::new();
    for _ in 0..40 {
        if fake.opened().len() == opened {
            return old;
        }
        old.extend(step(session, fake).0);
    }
    panic!("the stream never opened again");
}

/// The largest step between two frames of `out` on any of its `channels`.
fn largest_step(out: &[f32], channels: usize) -> f32 {
    let frames: Vec<&[f32]> = out.chunks_exact(channels).collect();
    frames
        .windows(2)
        .flat_map(|pair| (0..channels).map(move |c| (pair[1][c] - pair[0][c]).abs()))
        .fold(0.0, f32::max)
}

/// A hard edge would step by the whole band; a ramp of the 256-frame
/// declick steps by a 256th of it, and the routing crossfade by a 160th.
const RAMP_STEP: f32 = ALL / 64.0;

#[test]
fn a_routing_that_needs_more_outputs_ramps_out_reopens_the_stream_and_ramps_back_in() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::stereo(8)));
    assert_eq!(fake.opened()[0].channels, 2);
    let out = play(&mut session, &fake, 512);
    assert_eq!(last(&out, 2), [ALL, ALL]);
    let before = session.poll().source_frame;
    session.set_routing(Routing::preset(8)).unwrap();
    assert_eq!(fake.opened().len(), 1, "the stream ramps out first");

    let old = until_reopened(&mut session, &fake, 2);
    assert_eq!(fake.opened()[1].channels, 8);
    assert_eq!(last(&old, 2), [0.0, 0.0], "silent when the stream closes");
    let edge = largest_step(&[&out[out.len() - 2..], &old[..]].concat(), 2);
    assert!(edge <= RAMP_STEP, "the old stream steps by {edge}");
    let held = session.poll().source_frame;
    assert!(held > before, "the playhead held where the ramp ended");

    let new = run(&mut session, &fake, 2_048);
    assert!(
        new[..8].iter().all(|sample| sample.abs() <= RAMP_STEP),
        "the new stream starts from silence: {:?}",
        &new[..8]
    );
    let edge = largest_step(&new, 8);
    assert!(edge <= RAMP_STEP, "the new stream steps by {edge}");
    assert_eq!(
        last(&new, 8),
        [GUIDE, DRUMS, BASS, GUITAR, KEYS, KEYS, VOCALS, VOCALS]
    );
    let snapshot = session.poll();
    assert_eq!(snapshot.transport, TransportState::Playing);
    assert!(snapshot.source_frame > held, "it plays on from there");
}

#[test]
fn a_buffer_size_changed_while_playing_ramps_out_and_back_in() {
    let dir = band();
    let fake = device(2);
    let mut session = open(dir.path(), &fake, None);
    let out = play(&mut session, &fake, 512);
    session.set_buffer_frames(512).unwrap();
    assert_eq!(fake.opened().len(), 1, "the stream ramps out first");
    let old = until_reopened(&mut session, &fake, 2);
    assert_eq!(fake.opened()[1].buffer_frames, 512);
    assert_eq!(last(&old, 2), [0.0, 0.0]);
    let edge = largest_step(&[&out[out.len() - 2..], &old[..]].concat(), 2);
    assert!(edge <= RAMP_STEP, "the old stream steps by {edge}");
    let new = run(&mut session, &fake, 2_048);
    assert!(new[..2].iter().all(|sample| sample.abs() <= RAMP_STEP));
    assert!(largest_step(&new, 2) <= RAMP_STEP);
    assert_eq!(last(&new, 2), [ALL, ALL]);
    assert_eq!(session.poll().transport, TransportState::Playing);
}

#[test]
fn a_pause_sent_while_the_stream_ramps_out_keeps_it_paused_once_it_opens_again() {
    let dir = band();
    let fake = device(2);
    let mut session = open(dir.path(), &fake, None);
    play(&mut session, &fake, 512);
    session.set_buffer_frames(512).unwrap();
    session.send(Command::Pause).unwrap();
    until_reopened(&mut session, &fake, 2);
    let out = run(&mut session, &fake, 1_024);
    assert!(out.iter().all(|&sample| sample == 0.0));
    let paused = session.poll();
    assert_eq!(paused.transport, TransportState::Paused);

    // Play from there plays on.
    let out = play(&mut session, &fake, 1_024);
    assert_eq!(last(&out, 2), [ALL, ALL]);
    assert!(session.poll().source_frame > paused.source_frame);
}

#[test]
fn a_paused_session_reopens_at_once_and_stays_paused() {
    let dir = band();
    let fake = device(2);
    let mut session = open(dir.path(), &fake, None);
    play(&mut session, &fake, 512);
    session.send(Command::Pause).unwrap();
    run(&mut session, &fake, 1_024);
    let paused = session.poll();
    assert_eq!(paused.transport, TransportState::Paused);
    session.set_buffer_frames(128).unwrap();
    assert_eq!(fake.opened().len(), 2, "nothing to ramp out");
    let out = run(&mut session, &fake, 1_024);
    assert!(out.iter().all(|&sample| sample == 0.0));
    let still = session.poll();
    assert_eq!(still.transport, TransportState::Paused);
    assert_eq!(still.source_frame, paused.source_frame);
}

/// Unplugs the device, polls until the session holds the player, plugs it
/// back with `outputs` outputs and polls until it plays again.
fn unplug_and_replug(session: &mut Session, fake: &FakeOutput, outputs: u16) -> Snapshot {
    fake.unplug("fake:1");
    let lost = session.poll();
    assert!(matches!(lost.device, DeviceStatus::Disconnected { .. }));
    assert_eq!(lost.transport, TransportState::Paused);
    fake.set_channels("fake:1", outputs);
    fake.plug("fake:1");
    for _ in 0..100 {
        let snapshot = session.poll();
        if matches!(snapshot.device, DeviceStatus::Connected(_)) {
            return snapshot;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the device never came back");
}

#[test]
fn plugging_the_device_back_in_restores_the_routing() {
    let dir = band();
    let fake = device(8);
    let routing = preset_with(8, Bus::Drums, Destination::Stereo { left: 5, right: 6 });
    let mut session = open(dir.path(), &fake, Some(routing.clone()));
    play(&mut session, &fake, 512);
    let back = unplug_and_replug(&mut session, &fake, 8);
    assert_eq!(back.mixer.routing, routing);
    assert_eq!(back.last_error, None);
    assert_eq!(fake.opened()[1].channels, 8);
    let out = play(&mut session, &fake, 1_024);
    assert_eq!(
        last(&out, 8),
        [
            GUIDE,
            0.0,
            BASS,
            GUITAR,
            KEYS + DRUMS,
            KEYS + DRUMS,
            VOCALS,
            VOCALS
        ]
    );
}

#[test]
fn a_device_back_with_fewer_outputs_gets_the_routing_refitted_and_says_so() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::preset(8)));
    play(&mut session, &fake, 512);
    let back = unplug_and_replug(&mut session, &fake, 2);
    let fitted = Routing::preset(8).fit(2);
    assert_eq!(back.mixer.routing, fitted);
    let error = RuntimeError::RoutingRefitted {
        device: "Interface".to_owned(),
        outputs: 2,
    };
    assert_eq!(back.last_error, Some(error.clone()));
    assert_eq!(
        error.to_string(),
        "audio device \"Interface\" came back with 2 output(s); the routing was fitted to them"
    );
    assert_eq!(fake.opened()[1].channels, 2);
    let out = play(&mut session, &fake, 1_024);
    // Click & Guide stays on 1 and Drums on 2; the rest falls back to 1/2.
    let rest = BASS + GUITAR + KEYS + VOCALS;
    assert_eq!(last(&out, 2), [GUIDE + rest, DRUMS + rest]);
}

#[test]
fn every_setlist_item_plays_with_the_routing_and_a_change_reaches_the_cued_one() {
    let dir = tempfile::tempdir().unwrap();
    write_band(&dir.path().join("first"));
    write_band(&dir.path().join("second"));
    let library = Library::new(dir.path());
    let mut setlist = Setlist::new("Sunday");
    setlist.items = ["first", "second"]
        .into_iter()
        .map(|path| {
            SetlistItem::Song(SongItem {
                transition: Transition::AutoLink,
                ..SongItem::new(path)
            })
        })
        .collect();
    let file = library.save_setlist(&setlist).unwrap().file;
    let fake = device(8);
    let mut session = SetlistSession::open(
        &library,
        &file,
        Box::new(fake.clone()),
        options(Some(Routing::preset(8))),
    )
    .unwrap();
    let step = |session: &mut SetlistSession| {
        for monitor in session.disk_monitors() {
            assert!(monitor.wait_idle(Duration::from_secs(10)));
        }
        let out = fake.render(256).expect("a stream plays");
        (out, session.poll())
    };
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    let mut out = Vec::new();
    assert!(
        (0..100).any(|_| step(&mut session).1.setlist.next_ready),
        "the second song is cued"
    );
    // The second song is cued when the routing changes: Drums join Bass.
    let routing = preset_with(8, Bus::Drums, Destination::Mono(3));
    session.set_routing(routing.clone()).unwrap();
    for _ in 0..(FRAMES + 2_048) / 256 {
        let (frames, snapshot) = step(&mut session);
        out = frames;
        if snapshot.setlist.current == 1 && snapshot.song.output_frame > 1_024 {
            assert_eq!(snapshot.song.mixer.routing, routing);
            break;
        }
    }
    assert_eq!(
        last(&out, 8),
        [GUIDE, 0.0, BASS + DRUMS, GUITAR, KEYS, KEYS, VOCALS, VOCALS],
        "the second song plays with the new routing"
    );
    assert_eq!(fake.opened().len(), 1, "one stream for both songs");
}

#[test]
fn a_mixer_command_the_queue_refuses_still_reaches_the_audio_or_changes_nothing() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::preset(8)));
    play(&mut session, &fake, 512);
    let refused = (0..64).find(|k| {
        let db = if k % 2 == 0 { 12.0 } else { -40.0 };
        session
            .send(Command::SetMasterVolume(Volume::from_db(db)))
            .is_err()
    });
    assert!(refused.is_some(), "the command queue filled");
    run(&mut session, &fake, 4_096);
    let snapshot = session.poll();
    let master = snapshot.mixer.buses.master_volume.gain() as f32;
    for stem in &snapshot.mixer.stems {
        assert!(
            (stem.gain - master).abs() < 1e-6,
            "{} plays {} while the master shows {master}",
            stem.id,
            stem.gain
        );
    }
}

/// A setlist of two band songs linked by AutoLink, on `fake`.
fn two_songs(dir: &Path, fake: &FakeOutput) -> SetlistSession {
    write_band(&dir.join("first"));
    write_band(&dir.join("second"));
    let library = Library::new(dir);
    let mut setlist = Setlist::new("Sunday");
    setlist.items = ["first", "second"]
        .into_iter()
        .map(|path| {
            SetlistItem::Song(SongItem {
                transition: Transition::AutoLink,
                ..SongItem::new(path)
            })
        })
        .collect();
    let file = library.save_setlist(&setlist).unwrap().file;
    SetlistSession::open(&library, &file, Box::new(fake.clone()), options(None)).unwrap()
}

fn setlist_step(
    session: &mut SetlistSession,
    fake: &FakeOutput,
) -> (Vec<f32>, app::SetlistSnapshot) {
    for monitor in session.disk_monitors() {
        assert!(monitor.wait_idle(Duration::from_secs(10)));
    }
    let out = fake.render(256).expect("a stream plays");
    (out, session.poll())
}

#[test]
fn a_refused_mixer_command_still_reaches_the_cued_setlist_item() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeOutput::new();
    let mut session = two_songs(dir.path(), &fake);
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    assert!(
        (0..100).any(|_| setlist_step(&mut session, &fake).1.setlist.next_ready),
        "the second song is cued"
    );
    let refused = (0..64).find(|k| {
        let db = if k % 2 == 0 { 12.0 } else { -40.0 };
        session
            .send(SetlistCommand::Item(Command::SetMasterVolume(
                Volume::from_db(db),
            )))
            .is_err()
    });
    assert!(refused.is_some(), "the command queue filled");
    let master = session.bus_settings().master_volume.gain() as f32;
    let mut out = Vec::new();
    for _ in 0..16 {
        out = setlist_step(&mut session, &fake).0;
    }
    assert!(
        (last(&out, 2)[0] - ALL * master).abs() < 1e-5,
        "the first song plays {:?}, the master shows {master}",
        last(&out, 2)
    );
    let mut snapshot = session.poll();
    for _ in 0..FRAMES / 256 + 16 {
        let (frames, next) = setlist_step(&mut session, &fake);
        out = frames;
        snapshot = next;
        if snapshot.setlist.current == 1 && snapshot.song.output_frame > 1_024 {
            break;
        }
    }
    assert_eq!(snapshot.setlist.current, 1, "the second song plays");
    for stem in &snapshot.song.mixer.stems {
        assert!(
            (stem.gain - master).abs() < 1e-6,
            "{} plays {} while the master shows {master}",
            stem.id,
            stem.gain
        );
    }
    assert!(
        (last(&out, 2)[0] - ALL * master).abs() < 1e-5,
        "the second song plays {:?}, the master shows {master}",
        last(&out, 2)
    );
}

/// Sends commands the audio thread has not taken yet until its queue is
/// full.
fn fill_queue(session: &mut Session) {
    for _ in 0..1_000 {
        if session
            .send(Command::SetJumpTiming(JumpTiming::NextBar))
            .is_err()
        {
            return;
        }
    }
    panic!("the queue never filled");
}

#[test]
fn a_routing_refitted_while_the_queue_is_full_still_reaches_the_players() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::preset(8)));
    // The interface is set up with two outputs while the session runs.
    fake.set_channels("fake:1", 2);
    fill_queue(&mut session);
    // Stopped, so the stream opens again at once, on two outputs.
    session.set_buffer_frames(512).unwrap();
    assert_eq!(fake.opened()[1].channels, 2);
    run(&mut session, &fake, 256);
    assert_eq!(session.poll().mixer.routing, Routing::preset(8).fit(2));
    let out = play(&mut session, &fake, 1_024);
    // Click & Guide stays on 1 and Drums on 2; the rest falls back to 1/2.
    let rest = BASS + GUITAR + KEYS + VOCALS;
    assert_eq!(last(&out, 2), [GUIDE + rest, DRUMS + rest]);
}

#[test]
fn a_device_back_with_its_outputs_again_plays_its_routing_without_a_notice() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::preset(8)));
    play(&mut session, &fake, 512);
    let fewer = unplug_and_replug(&mut session, &fake, 2);
    assert!(
        matches!(
            fewer.last_error,
            Some(RuntimeError::RoutingRefitted { outputs: 2, .. })
        ),
        "{:?}",
        fewer.last_error
    );
    let back = unplug_and_replug(&mut session, &fake, 8);
    assert_eq!(back.mixer.routing, Routing::preset(8), "the whole routing");
    assert_eq!(back.last_error, None, "restoring it is not a refit");
    assert_eq!(
        fake.opened().last().map(|request| request.channels),
        Some(8)
    );
}

#[test]
fn a_reopen_asked_while_the_queue_is_full_still_ramps_out_first() {
    let dir = band();
    let fake = device(2);
    let mut session = open(dir.path(), &fake, None);
    let out = play(&mut session, &fake, 512);
    fill_queue(&mut session);
    session.set_buffer_frames(512).unwrap();
    assert_eq!(fake.opened().len(), 1, "never mid-signal");
    let old = until_reopened(&mut session, &fake, 2);
    assert_eq!(last(&old, 2), [0.0, 0.0]);
    let edge = largest_step(&[&out[out.len() - 2..], &old[..]].concat(), 2);
    assert!(edge <= RAMP_STEP, "the old stream steps by {edge}");
    let new = run(&mut session, &fake, 2_048);
    assert!(new[..2].iter().all(|sample| sample.abs() <= RAMP_STEP));
    assert!(largest_step(&new, 2) <= RAMP_STEP);
    assert_eq!(last(&new, 2), [ALL, ALL]);
    assert_eq!(session.poll().transport, TransportState::Playing);
}

#[test]
fn a_routing_asked_while_the_queue_is_full_reaches_the_players_with_a_ramp() {
    let dir = band();
    let fake = device(8);
    let mut session = open(dir.path(), &fake, Some(Routing::stereo(8)));
    let out = play(&mut session, &fake, 512);
    fill_queue(&mut session);
    let error = session.set_routing(Routing::preset(8)).unwrap_err();
    assert!(matches!(error, RuntimeError::QueueFull { .. }), "{error:?}");
    assert_eq!(
        session.poll().mixer.routing,
        Routing::stereo(8),
        "not in use before the players have it"
    );
    assert_eq!(fake.opened().len(), 1, "never mid-signal");
    let old = until_reopened(&mut session, &fake, 2);
    assert_eq!(fake.opened()[1].channels, 8);
    assert_eq!(last(&old, 2), [0.0, 0.0]);
    let edge = largest_step(&[&out[out.len() - 2..], &old[..]].concat(), 2);
    assert!(edge <= RAMP_STEP, "the old stream steps by {edge}");
    let new = run(&mut session, &fake, 2_048);
    assert!(new[..8].iter().all(|sample| sample.abs() <= RAMP_STEP));
    assert_eq!(
        last(&new, 8),
        [GUIDE, DRUMS, BASS, GUITAR, KEYS, KEYS, VOCALS, VOCALS]
    );
    let snapshot = session.poll();
    assert_eq!(snapshot.mixer.routing, Routing::preset(8));
    assert_eq!(snapshot.transport, TransportState::Playing);
}

#[test]
fn a_pause_the_queue_refused_does_not_keep_a_reopen_waiting() {
    let dir = band();
    let fake = device(2);
    let mut session = open(dir.path(), &fake, None);
    play(&mut session, &fake, 512);
    fill_queue(&mut session);
    assert!(
        session.send(Command::Pause).is_err(),
        "the pause is refused"
    );
    session.set_buffer_frames(512).unwrap();
    let old = until_reopened(&mut session, &fake, 2);
    assert_eq!(last(&old, 2), [0.0, 0.0], "it ramped out");
    let new = run(&mut session, &fake, 2_048);
    assert_eq!(last(&new, 2), [ALL, ALL], "and plays on, as before");
    assert_eq!(session.poll().transport, TransportState::Playing);
}

#[test]
fn a_buffer_size_set_while_the_device_is_away_opens_with_it_when_it_is_back() {
    let dir = band();
    let fake = device(2);
    let mut session = open(dir.path(), &fake, None);
    play(&mut session, &fake, 512);
    fake.unplug("fake:1");
    assert!(matches!(
        session.poll().device,
        DeviceStatus::Disconnected { .. }
    ));
    let attempts = fake.open_attempts();
    session.set_buffer_frames(512).unwrap();
    assert_eq!(fake.open_attempts(), attempts, "nothing opens meanwhile");
    fake.plug("fake:1");
    let back = session.poll();
    let DeviceStatus::Connected(info) = &back.device else {
        panic!("the device plays again: {:?}", back.device);
    };
    assert_eq!(info.buffer_frames, 512);
    assert_eq!(back.transport, TransportState::Paused, "as after any loss");
}

#[test]
fn a_buffer_size_the_device_refuses_holds_the_session_until_one_it_takes() {
    let dir = band();
    let fake = device(2);
    fake.set_buffer_range("fake:1", Some((64, 512)));
    let mut session = open(dir.path(), &fake, None);
    play(&mut session, &fake, 512);
    session.set_buffer_frames(1_024).unwrap();
    let mut held = session.poll();
    for _ in 0..40 {
        if matches!(held.device, DeviceStatus::Disconnected { .. }) {
            break;
        }
        assert!(session.disk_monitor().wait_idle(Duration::from_secs(10)));
        fake.render(256);
        held = session.poll();
    }
    let refused = OutputError::UnsupportedBufferSize {
        device: "Interface".to_owned(),
        requested: 1_024,
        min: 64,
        max: 512,
    };
    assert_eq!(
        held.device,
        DeviceStatus::Disconnected {
            device: "Interface".to_owned(),
            reason: LossReason::Reconfigured,
            reopen_error: Some(refused.clone()),
        }
    );
    assert_eq!(
        held.last_error,
        Some(RuntimeError::DeviceReopenFailed {
            device: "Interface".to_owned(),
            error: refused,
        })
    );
    assert_eq!(held.transport, TransportState::Paused, "it ramped out");
    assert_eq!(
        LossReason::Reconfigured.to_string(),
        "the stream closed to open again with new settings"
    );

    // A size the device takes opens it again, paused as after any loss.
    session.set_buffer_frames(512).unwrap();
    let back = session.poll();
    let DeviceStatus::Connected(info) = &back.device else {
        panic!("the device plays again: {:?}", back.device);
    };
    assert_eq!(info.buffer_frames, 512);
    assert_eq!(back.last_error, None);
    assert_eq!(back.transport, TransportState::Paused);
    let out = play(&mut session, &fake, 1_024);
    assert_eq!(last(&out, 2), [ALL, ALL]);
}
