//! T6b through the `app` API: the level meters a shell draws beside every
//! stem fader, every bus fader and the master. The engine reports peaks; the
//! session turns them into meters with a fast attack, a 20 dB/s decay and a
//! peak hold ([`MeterBallistics`]).

use std::fs;
use std::path::Path;
use std::time::Duration;

use app::testing::FakeOutput;
use app::{
    Command, METER_DECAY_DB_PER_SECOND, METER_FLOOR, METER_HOLD, Meter, MeterBallistics, Session,
    SessionOptions, SetlistCommand, SetlistSession, Snapshot,
};
use domain::{
    Arrangement, Bus, SectionMarker, SectionType, Setlist, SetlistItem, SongItem, SongPackage,
    Stem, StemRole, TempoPoint, TimeSignaturePoint, Transition,
};
use engine::live::MAX_METERED_STEMS;
use hound::{SampleFormat, WavSpec, WavWriter};
use library::Library;

const RATE: u32 = 8_000;
/// Four seconds.
const FRAMES: usize = 4 * RATE as usize;

/// The decay over `elapsed`, as a factor.
fn fall(elapsed: Duration) -> f32 {
    10f32.powf(-METER_DECAY_DB_PER_SECOND * elapsed.as_secs_f32() / 20.0)
}

#[track_caller]
fn close(got: [f32; 2], expected: [f32; 2]) {
    for (got, expected) in got.into_iter().zip(expected) {
        assert!(
            (got - expected).abs() <= expected.abs() * 1e-5 + 1e-9,
            "{got} instead of {expected}"
        );
    }
}

#[test]
fn a_meter_rises_at_once_falls_20_db_a_second_and_holds_its_peak_a_moment() {
    assert_eq!(METER_DECAY_DB_PER_SECOND, 20.0);
    assert_eq!(METER_HOLD, Duration::from_secs(1));
    let mut meter = MeterBallistics::default();
    assert_eq!(meter.meter(), Meter::default(), "silence to start with");

    // Attack: the peak shows at once.
    let peak = [0.5, 0.25];
    let shown = meter.update(peak, Duration::from_millis(33));
    assert_eq!(
        shown,
        Meter {
            level: peak,
            hold: peak
        }
    );

    // Decay: 20 dB in a second; the hold stays a second.
    let second = Duration::from_secs(1);
    let shown = meter.update([0.0, 0.0], second);
    close(shown.level, [0.05, 0.025]);
    assert_eq!(shown.hold, peak, "held for a second");

    // Past the hold, it falls at the same rate, never below the level.
    let half = Duration::from_millis(500);
    let shown = meter.update([0.0, 0.0], half);
    close(shown.level, [0.05 * fall(half), 0.025 * fall(half)]);
    close(shown.hold, [0.5 * fall(half), 0.25 * fall(half)]);

    // A louder peak rises at once and holds again; one below the falling
    // level changes nothing.
    let shown = meter.update([0.75, 0.000_1], Duration::from_millis(10));
    assert_eq!(shown.level[0], 0.75);
    assert_eq!(shown.hold[0], 0.75);
    // 0.25 has fallen for 1.51 s.
    let expected = 0.25 * fall(Duration::from_millis(1_510));
    close([shown.level[1], 0.0], [expected, 0.0]);

    // Long silence reads silence, not a tiny number.
    let shown = meter.update([0.0, 0.0], Duration::from_secs(10));
    assert_eq!(shown, Meter::default());
    // Just above the floor (−100 dBFS) still reads; just below is silence.
    let mut quiet = MeterBallistics::default();
    let above = METER_FLOOR * 1.5;
    assert_eq!(
        quiet
            .update([above, METER_FLOOR / 2.0], Duration::ZERO)
            .level,
        [above, 0.0]
    );
}

#[test]
fn a_hold_that_starts_falling_during_a_step_falls_only_past_the_hold() {
    let mut meter = MeterBallistics::default();
    meter.update([0.5, 0.5], Duration::ZERO);
    meter.update([0.0, 0.0], Duration::from_millis(800));
    // 1.3 s since the peak: 0.3 s of falling.
    let shown = meter.update([0.0, 0.0], Duration::from_millis(500));
    let falling = Duration::from_millis(300);
    close(shown.hold, [0.5 * fall(falling), 0.5 * fall(falling)]);
}

/// Writes a song of `stems` (id, role, level) held constant, plus a silent
/// click stem.
fn write_song(dir: &Path, stems: &[(String, StemRole, f32)]) {
    let mut all = stems.to_vec();
    all.push(("click".to_owned(), StemRole::Click, 0.0));
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
        stems: all
            .iter()
            .map(|(id, role, _)| Stem {
                id: id.clone(),
                file: format!("stems/{id}.wav"),
                part: id.clone(),
                role: *role,
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
    for (id, _, level) in &all {
        let mut writer = WavWriter::create(dir.join(format!("stems/{id}.wav")), spec).unwrap();
        for _ in 0..FRAMES {
            writer.write_sample(*level).unwrap();
        }
        writer.finalize().unwrap();
    }
}

/// Drums 0.5 (Drums), bass 0.25 (Bass), guide 0.125 (Click & Guide).
fn band() -> Vec<(String, StemRole, f32)> {
    vec![
        ("drums".to_owned(), StemRole::Track, 0.5),
        ("bass".to_owned(), StemRole::Track, 0.25),
        ("guide".to_owned(), StemRole::Guide, 0.125),
    ]
}

fn step(session: &mut Session, fake: &FakeOutput) -> Snapshot {
    assert!(session.disk_monitor().wait_idle(Duration::from_secs(10)));
    fake.render(256).expect("a stream plays");
    session.poll()
}

/// Plays until the transport runs, then `callbacks` more callbacks.
fn play(session: &mut Session, fake: &FakeOutput, callbacks: usize) -> Snapshot {
    session.send(Command::Play).unwrap();
    for _ in 0..100 {
        if step(session, fake).output_frame > 0 {
            let mut snapshot = session.poll();
            for _ in 0..callbacks {
                snapshot = step(session, fake);
            }
            return snapshot;
        }
    }
    panic!("the transport never started");
}

fn level(meter: Option<Meter>) -> [f32; 2] {
    meter.expect("the stem has a meter").level
}

#[test]
fn the_snapshot_shows_a_meter_for_every_stem_bus_and_the_master() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), &band());
    let fake = FakeOutput::new();
    let mut session = Session::open(
        dir.path(),
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    let stopped = session.poll().mixer;
    assert!(
        stopped
            .stems
            .iter()
            .all(|stem| stem.meter == Some(Meter::default())),
        "a stopped session reads silence"
    );
    assert_eq!(stopped.master_meter, Meter::default());

    session
        .send(Command::SetStemMute {
            stem: 1,
            mute: true,
        })
        .unwrap();
    let mixer = play(&mut session, &fake, 8).mixer;
    assert_eq!(level(mixer.stems[0].meter), [0.5, 0.5]);
    assert_eq!(level(mixer.stems[1].meter), [0.0, 0.0], "the muted bass");
    assert_eq!(
        mixer.stems[1].meter.map(|meter| meter.hold),
        Some([0.0, 0.0])
    );
    assert_eq!(level(mixer.stems[2].meter), [0.125, 0.125]);
    assert_eq!(mixer.bus_meters[Bus::Drums.index()].level, [0.5, 0.5]);
    assert_eq!(mixer.bus_meters[Bus::Bass.index()].level, [0.0, 0.0]);
    assert_eq!(
        mixer.bus_meters[Bus::ClickGuide.index()].level,
        [0.125, 0.125]
    );
    assert_eq!(mixer.master_meter.level, [0.625, 0.625]);
    assert_eq!(mixer.master_meter.hold, [0.625, 0.625]);
}

#[test]
fn stems_past_the_meters_maximum_have_no_meter() {
    let dir = tempfile::tempdir().unwrap();
    let stems: Vec<(String, StemRole, f32)> = (0..MAX_METERED_STEMS + 2)
        .map(|i| (format!("pad{i}"), StemRole::Track, 1.0 / 256.0))
        .collect();
    write_song(dir.path(), &stems);
    let fake = FakeOutput::new();
    let mut session = Session::open(
        dir.path(),
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    let mixer = play(&mut session, &fake, 8).mixer;
    let metered = mixer
        .stems
        .iter()
        .filter(|stem| stem.meter.is_some())
        .count();
    assert_eq!(metered, MAX_METERED_STEMS);
    assert!(
        mixer.stems[MAX_METERED_STEMS..]
            .iter()
            .all(|stem| stem.meter.is_none())
    );
    let all = (MAX_METERED_STEMS + 2) as f32 / 256.0;
    assert_eq!(
        mixer.master_meter.level,
        [all, all],
        "every stem in the master"
    );
}

#[test]
fn a_setlist_shows_the_meters_of_the_item_that_plays() {
    let dir = tempfile::tempdir().unwrap();
    write_song(&dir.path().join("first"), &band());
    write_song(
        &dir.path().join("second"),
        &[("vocals".to_owned(), StemRole::Track, 0.0625)],
    );
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
    let fake = FakeOutput::new();
    let mut session = SetlistSession::open(
        &library,
        &file,
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    let step = |session: &mut SetlistSession| {
        for monitor in session.disk_monitors() {
            assert!(monitor.wait_idle(Duration::from_secs(10)));
        }
        fake.render(256).expect("a stream plays");
        session.poll()
    };
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    let mut snapshot = step(&mut session);
    for _ in 0..16 {
        snapshot = step(&mut session);
    }
    assert_eq!(snapshot.song.mixer.stems.len(), 4, "the first song's stems");
    assert_eq!(level(snapshot.song.mixer.stems[0].meter), [0.5, 0.5]);

    for _ in 0..FRAMES / 256 + 16 {
        snapshot = step(&mut session);
        if snapshot.setlist.current == 1 && snapshot.song.output_frame > 2_048 {
            break;
        }
    }
    assert_eq!(snapshot.setlist.current, 1);
    let mixer = &snapshot.song.mixer;
    assert_eq!(mixer.stems.len(), 2, "the second song's stems");
    assert_eq!(level(mixer.stems[0].meter), [0.0625, 0.0625]);
    assert_eq!(
        mixer.stems[0].meter.map(|meter| meter.hold),
        Some([0.0625, 0.0625]),
        "its own hold, not the first song's"
    );
    assert_eq!(
        mixer.bus_meters[Bus::Vocals.index()].level,
        [0.0625, 0.0625]
    );
    // The first song's drums fall from 0.5 at 20 dB/s.
    let drums = mixer.bus_meters[Bus::Drums.index()].level[0];
    assert!(drums > 0.0 && drums < 0.5, "{drums}");
}

#[test]
fn a_lost_device_reads_falling_meters_not_the_last_peak() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), &band());
    let fake = FakeOutput::new();
    let mut session = Session::open(
        dir.path(),
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    let playing = play(&mut session, &fake, 8).mixer.master_meter.level[0];
    assert_eq!(playing, 0.875);
    fake.unplug(app::testing::FAKE_DEVICE_ID);
    session.poll();
    std::thread::sleep(Duration::from_millis(100));
    let lost = session.poll().mixer.master_meter.level[0];
    assert!(lost < playing * fall(Duration::from_millis(90)), "{lost}");
}
