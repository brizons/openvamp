//! T6b through the `app` API: the level meters a shell draws beside every
//! stem fader, every bus fader and the master. The engine reports peaks; the
//! session turns them into meters with a fast attack, a 20 dB/s decay and a
//! peak hold ([`MeterBallistics`]).

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use app::testing::FakeOutput;
use app::{
    Command, METER_DECAY_DB_PER_SECOND, METER_FLOOR, METER_HOLD, Meter, MeterBallistics, Session,
    SessionOptions, SetlistCommand, SetlistSession, Snapshot, TransportState,
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

/// What a call returned, and the instants just before and after it: the
/// session's own clock read something in between.
struct Timed<T> {
    value: T,
    start: Instant,
    end: Instant,
}

fn timed<T>(call: impl FnOnce() -> T) -> Timed<T> {
    let start = Instant::now();
    let value = call();
    Timed {
        value,
        start,
        end: Instant::now(),
    }
}

/// The least and the most time the meters saw between two timed polls:
/// wall-clock assertions are bounded by these, never by a sleep's length,
/// which a loaded machine overruns.
fn between<A, B>(first: &Timed<A>, second: &Timed<B>) -> (Duration, Duration) {
    (
        second.start.saturating_duration_since(first.end),
        second.end.saturating_duration_since(first.start),
    )
}

/// Asserts a meter fell from `from` to `to` at [`METER_DECAY_DB_PER_SECOND`]
/// over some time within `(least, most)`.
#[track_caller]
fn assert_fell(from: f32, to: f32, (least, most): (Duration, Duration), what: &str) {
    let fell = 20.0 * (to / from).log10();
    let rate = METER_DECAY_DB_PER_SECOND;
    let (slowest, fastest) = (-rate * least.as_secs_f32(), -rate * most.as_secs_f32());
    assert!(
        fell <= slowest + 0.05 && fell >= fastest - 0.05,
        "{what}: fell {fell} dB, expected {fastest} to {slowest} dB"
    );
}

/// Asserts a hold that last took `peak` at most `since` ago: still `peak`
/// within [`METER_HOLD`], falling but never below `level` after it (a
/// machine loaded enough to stretch the test past the hold).
#[track_caller]
fn assert_held(hold: f32, peak: f32, level: f32, since: Duration, what: &str) {
    if since < METER_HOLD {
        assert_eq!(hold, peak, "{what}: the hold stays its second");
    } else {
        assert!(hold <= peak && hold >= level, "{what}: hold {hold}");
    }
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
    write_song_at(dir, stems, FRAMES, RATE);
}

/// [`write_song`] with `frames` frames at `rate`.
fn write_song_at(dir: &Path, stems: &[(String, StemRole, f32)], frames: usize, rate: u32) {
    let mut all = stems.to_vec();
    all.push(("click".to_owned(), StemRole::Click, 0.0));
    let package = SongPackage {
        format_version: 1,
        title: "Band".to_owned(),
        artist: None,
        sample_rate: rate,
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
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    for (id, _, level) in &all {
        let mut writer = WavWriter::create(dir.join(format!("stems/{id}.wav")), spec).unwrap();
        for _ in 0..frames {
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

/// Stems named and levelled as given.
fn stems(stems: &[(&str, StemRole, f32)]) -> Vec<(String, StemRole, f32)> {
    stems
        .iter()
        .map(|&(id, role, level)| (id.to_owned(), role, level))
        .collect()
}

/// A setlist of the songs `first` and `second` in `dir`, both with
/// `transition`, on a fresh fake output.
fn two_items(dir: &Path, transition: Transition) -> (SetlistSession, FakeOutput) {
    let library = Library::new(dir);
    let mut setlist = Setlist::new("Sunday");
    setlist.items = ["first", "second"]
        .into_iter()
        .map(|path| {
            SetlistItem::Song(SongItem {
                transition,
                ..SongItem::new(path)
            })
        })
        .collect();
    let file = library.save_setlist(&setlist).unwrap().file;
    let fake = FakeOutput::new();
    let session = SetlistSession::open(
        &library,
        &file,
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    (session, fake)
}

fn setlist_step(session: &mut SetlistSession, fake: &FakeOutput) -> app::SetlistSnapshot {
    for monitor in session.disk_monitors() {
        assert!(monitor.wait_idle(Duration::from_secs(10)));
    }
    fake.render(256).expect("a stream plays");
    session.poll()
}

/// The first song, one second: drums 0.5 and a guide 0.25 (a guide plays
/// to the end); the second: two silent stems, so any level it shows is
/// another item's.
fn loud_then_silent(dir: &Path) {
    write_song_at(
        &dir.join("first"),
        &stems(&[
            ("drums", StemRole::Track, 0.5),
            ("guide", StemRole::Guide, 0.25),
        ]),
        RATE as usize,
        RATE,
    );
    write_song_at(
        &dir.join("second"),
        &stems(&[
            ("vocals", StemRole::Track, 0.0),
            ("keys", StemRole::Track, 0.0),
        ]),
        FRAMES,
        RATE,
    );
}

#[track_caller]
fn assert_stems_silent(snapshot: &app::SetlistSnapshot, when: &str) {
    for stem in &snapshot.song.mixer.stems {
        assert_eq!(
            stem.meter,
            Some(Meter::default()),
            "{when}: the new item's {} shows another item's level",
            stem.id
        );
    }
}

#[test]
fn after_cue_next_the_new_items_stem_meters_start_from_silence() {
    let dir = tempfile::tempdir().unwrap();
    loud_then_silent(dir.path());
    let (mut session, fake) = two_items(dir.path(), Transition::CueNext);
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    let mut snapshot = setlist_step(&mut session, &fake);
    for _ in 0..200 {
        if snapshot.setlist.current == 1 {
            break;
        }
        snapshot = setlist_step(&mut session, &fake);
    }
    assert_eq!(
        snapshot.setlist.current, 1,
        "Cue next selected the second song"
    );
    assert_eq!(snapshot.song.mixer.stems[0].id, "vocals");
    assert_stems_silent(&snapshot, "the poll that selected it");
    std::thread::sleep(Duration::from_millis(500));
    assert_stems_silent(&setlist_step(&mut session, &fake), "0.5 s later");
}

#[test]
fn a_selection_while_playing_starts_the_new_items_stem_meters_from_silence() {
    let dir = tempfile::tempdir().unwrap();
    loud_then_silent(dir.path());
    let (mut session, fake) = two_items(dir.path(), Transition::Stay);
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    let mut snapshot = setlist_step(&mut session, &fake);
    for _ in 0..12 {
        snapshot = setlist_step(&mut session, &fake);
    }
    assert_eq!(level(snapshot.song.mixer.stems[1].meter), [0.25, 0.25]);
    session.send(SetlistCommand::Select(1)).unwrap();
    // Polled before the audio thread took the selection.
    assert_stems_silent(&session.poll(), "before a callback");
    let mut last = setlist_step(&mut session, &fake);
    for _ in 0..3 {
        last = setlist_step(&mut session, &fake);
    }
    assert_eq!(last.setlist.current, 1);
    assert_stems_silent(&last, "after four callbacks");
}

#[test]
fn next_and_previous_start_the_new_items_stem_meters_from_silence() {
    let dir = tempfile::tempdir().unwrap();
    write_song(
        &dir.path().join("first"),
        &stems(&[("drums", StemRole::Track, 0.5)]),
    );
    write_song(
        &dir.path().join("second"),
        &stems(&[("vocals", StemRole::Track, 0.125)]),
    );
    let (mut session, fake) = two_items(dir.path(), Transition::Stay);
    for (command, playing) in [
        (SetlistCommand::Next, 0.5),
        (SetlistCommand::Previous, 0.125),
    ] {
        session.send(SetlistCommand::Item(Command::Play)).unwrap();
        let mut snapshot = setlist_step(&mut session, &fake);
        for _ in 0..12 {
            snapshot = setlist_step(&mut session, &fake);
        }
        assert_eq!(
            level(snapshot.song.mixer.stems[0].meter),
            [playing, playing],
            "{command:?}: the item playing"
        );
        session.send(command).unwrap();
        assert_stems_silent(&session.poll(), &format!("{command:?}"));
        assert_stems_silent(&setlist_step(&mut session, &fake), &format!("{command:?}"));
    }
}

#[test]
fn an_item_at_another_rate_starts_its_stem_meters_from_silence() {
    let dir = tempfile::tempdir().unwrap();
    write_song_at(
        &dir.path().join("first"),
        &stems(&[("drums", StemRole::Track, 0.5)]),
        RATE as usize,
        RATE,
    );
    write_song_at(
        &dir.path().join("second"),
        &stems(&[
            ("vocals", StemRole::Track, 0.0),
            ("keys", StemRole::Track, 0.0),
        ]),
        FRAMES,
        2 * RATE,
    );
    let (mut session, fake) = two_items(dir.path(), Transition::CueNext);
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    let mut snapshot = setlist_step(&mut session, &fake);
    for _ in 0..200 {
        if snapshot.setlist.current == 1 {
            break;
        }
        snapshot = setlist_step(&mut session, &fake);
    }
    assert_eq!(snapshot.setlist.current, 1, "the device reopened for it");
    assert_eq!(
        fake.opened().last().map(|request| request.sample_rate),
        Some(2 * RATE)
    );
    assert_stems_silent(&snapshot, "the poll that reopened the device");
    assert_stems_silent(&setlist_step(&mut session, &fake), "a callback later");
}

#[test]
fn a_device_back_from_a_loss_does_not_show_the_level_from_before_it() {
    let dir = tempfile::tempdir().unwrap();
    write_song_at(
        dir.path(),
        &stems(&[("drums", StemRole::Track, 0.5)]),
        8 * RATE as usize,
        RATE,
    );
    let fake = FakeOutput::new();
    let mut session = Session::open(
        dir.path(),
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    let loud = timed(|| play(&mut session, &fake, 20));
    assert_eq!(loud.value.mixer.master_meter.level, [0.5, 0.5]);
    fake.unplug(app::testing::FAKE_DEVICE_ID);
    let unplugged = timed(|| session.poll());
    // Well within the hold.
    std::thread::sleep(Duration::from_millis(300));
    let lost = timed(|| session.poll());
    let master = lost.value.mixer.master_meter;
    let level = unplugged.value.mixer.master_meter.level[0];
    assert_fell(level, master.level[0], between(&unplugged, &lost), "lost");
    assert_held(
        master.hold[0],
        0.5,
        master.level[0],
        between(&loud, &lost).1,
        "lost",
    );
    let before = session.poll().mixer.master_meter.level[0];
    fake.plug(app::testing::FAKE_DEVICE_ID);
    for _ in 0..50 {
        session.poll();
        if fake.is_streaming() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fake.is_streaming(), "the device plays again");
    let back = session.poll();
    assert!(
        back.mixer.master_meter.level[0] <= before,
        "back from a loss, paused, the master rose from {before} to {:?}",
        back.mixer.master_meter
    );
    for _ in 0..6 {
        let snapshot = step(&mut session, &fake);
        assert!(
            snapshot.mixer.master_meter.level[0] <= before,
            "{:?}",
            snapshot.mixer.master_meter
        );
    }
    std::thread::sleep(Duration::from_millis(400));
    let later = step(&mut session, &fake);
    assert!(
        later.mixer.master_meter.level[0] < 0.25,
        "a paused session back from a loss shows the level from before it: {:?}",
        later.mixer.master_meter
    );
}

/// One callback of `frames` frames once the disk has served what it may,
/// then a poll.
fn step_frames(session: &mut Session, fake: &FakeOutput, frames: usize) -> Snapshot {
    assert!(session.disk_monitor().wait_idle(Duration::from_secs(10)));
    fake.render(frames).expect("a stream plays");
    session.poll()
}

/// Pause, Stop and Panic: the poll after a short callback into the ramp
/// still takes the ramp's peaks, as the transport still runs. A meter fed
/// silence for the 300 ms before would read 0.5 at −6 dB, 0.25.
#[test]
fn a_ramp_out_is_metered_while_the_transport_still_runs() {
    for (halt, ramping) in [
        (Command::Pause, TransportState::Pausing),
        (Command::Stop, TransportState::Stopping),
        (Command::Panic, TransportState::Panicking),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_song_at(
            dir.path(),
            &stems(&[("drums", StemRole::Track, 0.5)]),
            8 * RATE as usize,
            RATE,
        );
        let fake = FakeOutput::new();
        let mut session = Session::open(
            dir.path(),
            Box::new(fake.clone()),
            SessionOptions::default(),
        )
        .unwrap();
        let snapshot = play(&mut session, &fake, 20);
        assert_eq!(snapshot.transport, TransportState::Playing);
        assert_eq!(snapshot.mixer.master_meter.level, [0.5, 0.5]);
        std::thread::sleep(Duration::from_millis(300));
        session.send(halt).unwrap();
        let snapshot = step_frames(&mut session, &fake, 16);
        assert_eq!(snapshot.transport, ramping, "{halt:?}");
        let mixer = &snapshot.mixer;
        let stem = level(mixer.stems[0].meter)[0];
        assert!(
            stem > 0.45,
            "{halt:?}: the stem was not fed the ramp: {stem}"
        );
        assert!(
            mixer.bus_meters[Bus::Drums.index()].level[0] > 0.45,
            "{halt:?}"
        );
        assert!(mixer.master_meter.level[0] > 0.45, "{halt:?}");
        // A panic ramps linearly over 500 ms of audio (4 000 frames):
        // 1 616 frames into it, the reading holds at least the last meter
        // window (400 frames), so the ramp's own peak, which depends only on
        // the frames rendered, is at least 0.5 times its gain 1 216 frames
        // in: 0.348. Silence for the wall-clock time between the polls
        // would read at most 0.5 at -6 dB, 0.25.
        if halt == Command::Panic {
            let into = timed(|| ());
            std::thread::sleep(Duration::from_millis(300));
            let later = timed(|| step_frames(&mut session, &fake, 1_600));
            assert_eq!(later.value.transport, TransportState::Panicking);
            let master = later.value.mixer.master_meter.level[0];
            let silence = 0.5 * fall(between(&into, &later).0);
            assert!(
                master > 0.34 && master > silence,
                "the panic ramp was not metered: {master} (silence: {silence})"
            );
        }
    }
}

/// AutoLink, Crossfade and Cue next: the first song's Drums bus and the
/// master carry on across the change, falling, with their hold; the new
/// item's stems read its own level once it plays (AutoLink, Crossfade) and
/// silence while it waits stopped (Cue next).
#[test]
fn under_every_handover_the_bus_and_master_meters_carry_on() {
    for transition in [
        Transition::AutoLink,
        Transition::Crossfade,
        Transition::CueNext,
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_song_at(
            &dir.path().join("first"),
            &stems(&[("drums", StemRole::Track, 0.5)]),
            3 * RATE as usize,
            RATE,
        );
        write_song_at(
            &dir.path().join("second"),
            &stems(&[("vocals", StemRole::Track, 0.125)]),
            6 * RATE as usize,
            RATE,
        );
        let (mut session, fake) = two_items(dir.path(), transition);
        session.send(SetlistCommand::Item(Command::Play)).unwrap();
        let mut snapshot = setlist_step(&mut session, &fake);
        let mut before = snapshot.clone();
        // When the Drums bus last read its full level: its hold's start.
        let mut loud = Instant::now();
        for _ in 0..400 {
            if snapshot.setlist.current == 1 {
                break;
            }
            before = snapshot.clone();
            let start = Instant::now();
            snapshot = setlist_step(&mut session, &fake);
            if snapshot.song.mixer.bus_meters[Bus::Drums.index()].level[0] == 0.5 {
                loud = start;
            }
        }
        assert_eq!(snapshot.setlist.current, 1, "{transition:?}");
        let drums_before = before.song.mixer.bus_meters[Bus::Drums.index()];
        let first = &snapshot.song.mixer;
        let drums_first = first.bus_meters[Bus::Drums.index()];
        assert!(
            drums_first.level[0] > 0.0,
            "{transition:?}: the Drums bus started again from silence"
        );
        assert_eq!(drums_first.hold, drums_before.hold, "{transition:?}");
        assert!(first.master_meter.level[0] > 0.0, "{transition:?}");

        std::thread::sleep(Duration::from_millis(300));
        let later = setlist_step(&mut session, &fake);
        let since = loud.elapsed();
        let mixer = &later.song.mixer;
        let drums = mixer.bus_meters[Bus::Drums.index()];
        assert!(
            drums.level[0] > 0.0 && drums.level[0] < drums_first.level[0],
            "{transition:?}: the Drums bus falls: {drums:?}"
        );
        assert_held(
            drums.hold[0],
            0.5,
            drums.level[0],
            since,
            &format!("{transition:?}"),
        );
        match transition {
            Transition::CueNext => {
                assert_eq!(later.song.transport, TransportState::Stopped);
                assert_eq!(level(mixer.stems[0].meter), [0.0, 0.0]);
            }
            _ => {
                assert_eq!(later.song.transport, TransportState::Playing);
                assert_eq!(
                    level(mixer.stems[0].meter),
                    [0.125, 0.125],
                    "{transition:?}: the incoming item is metered"
                );
                assert_eq!(mixer.bus_meters[Bus::Vocals.index()].level, [0.125, 0.125]);
                assert!(mixer.master_meter.level[0] >= 0.125, "{transition:?}");
            }
        }
    }
}

#[test]
fn a_paused_or_stopped_session_falls_to_silence_at_the_stated_rate() {
    for (halt, silent) in [
        (Command::Pause, TransportState::Paused),
        (Command::Stop, TransportState::Stopped),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_song(dir.path(), &band());
        let fake = FakeOutput::new();
        let mut session = Session::open(
            dir.path(),
            Box::new(fake.clone()),
            SessionOptions::default(),
        )
        .unwrap();
        let playing = timed(|| play(&mut session, &fake, 8));
        assert_eq!(playing.value.mixer.master_meter.level, [0.875, 0.875]);
        session.send(halt).unwrap();
        // Past the ramp out.
        let mut silenced = timed(|| step(&mut session, &fake));
        for _ in 0..3 {
            silenced = timed(|| step(&mut session, &fake));
        }
        assert_eq!(silenced.value.transport, silent, "{halt:?}");
        // Well within the hold.
        std::thread::sleep(Duration::from_millis(300));
        let later = timed(|| step(&mut session, &fake));
        let what = format!("{halt:?}");
        let interval = between(&silenced, &later);
        let (from, master) = (
            silenced.value.mixer.master_meter,
            later.value.mixer.master_meter,
        );
        assert_fell(from.level[0], master.level[0], interval, &what);
        let since = between(&playing, &later).1;
        assert_held(master.hold[0], 0.875, master.level[0], since, &what);
        let drums = (
            level(silenced.value.mixer.stems[0].meter)[0],
            level(later.value.mixer.stems[0].meter)[0],
        );
        assert_fell(
            drums.0,
            drums.1,
            interval,
            &format!("{what}: the drums stem"),
        );
        let bus = later.value.mixer.bus_meters[Bus::Drums.index()].level[0];
        assert_eq!(bus, drums.1, "{halt:?}: and its bus");
        // Not frozen: it keeps falling toward silence.
        std::thread::sleep(Duration::from_millis(50));
        let lower = step(&mut session, &fake).mixer.master_meter.level[0];
        assert!(lower < master.level[0], "{halt:?}: {lower}");
    }
}
