//! S1 through the `app` API: the mixer (F7.1-F7.5). Each song plays with
//! the mix of its arrangement from its `mix.json`; the bus and master
//! settings are global; every change ramps over 20 ms and reaches exactly
//! the stems `domain::effective_gains` says.
//!
//! The band song holds every stem at its own constant level, powers of two
//! apart, so the output says exactly which stems play and how loud.

use std::fs;
use std::path::Path;
use std::time::Duration;

use app::testing::FakeOutput;
use app::{
    Command, RuntimeError, Session, SessionOptions, SetlistCommand, SetlistSession, Snapshot,
};
use domain::{
    Arrangement, ArrangementMix, Bus, BusLevel, BusSettings, MixFile, MixValidationError,
    SectionMarker, SectionType, Setlist, SetlistItem, SongItem, SongPackage, Stem, StemMix,
    StemRole, TempoPoint, TimeSignaturePoint, Transition, Volume, effective_gains,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::{Library, read_mix, write_mix};

const RATE: u32 = 8_000;
/// Four seconds.
const FRAMES: usize = 4 * RATE as usize;
/// The 20 ms gain ramp at [`RATE`].
const RAMP: usize = 160;

/// The band: id, role and level of each stem, in package order. Drums and
/// percussion share the Drums bus.
const BAND: [(&str, StemRole, f32); 8] = [
    ("drums", StemRole::Track, 0.5),
    ("percussion", StemRole::Track, 0.003_906_25),
    ("bass", StemRole::Track, 0.25),
    ("keys", StemRole::Track, 0.125),
    ("guitar", StemRole::Track, 0.0625),
    ("vocals", StemRole::Track, 0.031_25),
    ("guide", StemRole::Guide, 0.015_625),
    ("click", StemRole::Click, 0.0),
];

const DRUMS: usize = 0;
const PERCUSSION: usize = 1;
const BASS: usize = 2;
const KEYS: usize = 3;
const GUITAR: usize = 4;
const GUIDE: usize = 6;

fn stems() -> Vec<Stem> {
    BAND.iter()
        .map(|&(id, role, _)| Stem {
            id: id.to_owned(),
            file: format!("stems/{id}.wav"),
            part: id.to_owned(),
            role,
        })
        .collect()
}

/// Writes the band song into `dir`, with arrangements `default` and `live`
/// (the same section), and `mix` as its `mix.json` when given.
fn write_band(dir: &Path, mix: Option<&MixFile>) {
    let arrangement = |id: &str| Arrangement {
        id: id.to_owned(),
        name: id.to_owned(),
        sections: vec!["verse".to_owned()],
        infinite_loop: Vec::new(),
    };
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
        stems: stems(),
        sections: vec![SectionMarker {
            id: "verse".to_owned(),
            section_type: SectionType::Verse,
            number: None,
            start: 0,
        }],
        arrangements: vec![arrangement("default"), arrangement("live")],
    };
    fs::create_dir_all(dir.join("stems")).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    for (id, _, level) in BAND {
        let mut writer = WavWriter::create(dir.join(format!("stems/{id}.wav")), spec).unwrap();
        for _ in 0..FRAMES {
            writer.write_sample(level).unwrap();
        }
        writer.finalize().unwrap();
    }
    if let Some(mix) = mix {
        fs::write(dir.join("mix.json"), mix.to_json().unwrap()).unwrap();
    }
}

fn band(mix: Option<&MixFile>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_band(dir.path(), mix);
    dir
}

/// A `mix.json` whose `arrangement` has `stems` set as given.
fn mix_file(arrangement: &str, stems: &[(&str, StemMix)]) -> MixFile {
    let mut mix = ArrangementMix::default();
    for (id, stem) in stems {
        mix.set_stem(*id, stem.clone());
    }
    let mut file = MixFile::new();
    file.set_arrangement(arrangement, mix);
    file
}

fn at(db: f64) -> StemMix {
    StemMix {
        volume: Volume::from_db(db),
        ..StemMix::default()
    }
}

fn muted() -> StemMix {
    StemMix {
        mute: true,
        ..StemMix::default()
    }
}

fn options() -> SessionOptions {
    SessionOptions::default()
}

fn open(dir: &Path, fake: &FakeOutput, options: SessionOptions) -> Session {
    Session::open(dir, Box::new(fake.clone()), options).expect("the session opens")
}

fn step(session: &mut Session, fake: &FakeOutput) -> (Vec<f32>, Snapshot) {
    assert!(session.disk_monitor().wait_idle(Duration::from_secs(10)));
    let out = fake.render(256).expect("a stream plays");
    (out, session.poll())
}

/// Plays until the transport runs, then `frames` more frames.
fn play(session: &mut Session, fake: &FakeOutput, frames: usize) -> Vec<f32> {
    session.send(Command::Play).unwrap();
    for _ in 0..100 {
        if step(session, fake).1.output_frame > 0 {
            return run(session, fake, frames);
        }
    }
    panic!("the transport never started");
}

fn run(session: &mut Session, fake: &FakeOutput, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    for _ in 0..frames.div_ceil(256) {
        out.extend(step(session, fake).0);
    }
    out
}

/// What the band plays on each of outputs 1 and 2 with these gains.
fn level(gains: &[f32]) -> f32 {
    BAND.iter()
        .zip(gains)
        .map(|(&(_, _, level), &gain)| level * gain)
        .sum()
}

fn gains_of(mix: &ArrangementMix, buses: &BusSettings) -> Vec<f32> {
    effective_gains(&stems(), mix, buses)
        .iter()
        .map(|gain| gain.gain)
        .collect()
}

#[track_caller]
fn assert_plays(out: &[f32], expected: f32) {
    let last = &out[out.len() - 2..];
    for &sample in last {
        assert!(
            (sample - expected).abs() <= 1e-6,
            "plays {last:?}, expected {expected}"
        );
    }
}

/// What a mixer command does to the mix and the bus settings.
type Change = dyn Fn(&mut ArrangementMix, &mut BusSettings);

#[test]
fn every_mixer_command_changes_exactly_the_gains_effective_gains_predicts_over_a_ramp() {
    let dir = band(None);
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    let mut out = play(&mut session, &fake, 512);
    let mut mix = ArrangementMix::default();
    let mut buses = BusSettings::new();
    let mut before = gains_of(&mix, &buses);
    assert_plays(&out, level(&before));

    let steps: Vec<(Command, Box<Change>)> = vec![
        (
            Command::SetStemVolume {
                stem: DRUMS,
                volume: Volume::from_db(-6.0),
            },
            Box::new(|mix, _| mix.set_stem("drums", at(-6.0))),
        ),
        (
            Command::SetStemMute {
                stem: BASS,
                mute: true,
            },
            Box::new(|mix, _| mix.set_stem("bass", muted())),
        ),
        (
            Command::SetStemSolo {
                stem: KEYS,
                solo: true,
            },
            Box::new(|mix, _| {
                mix.set_stem(
                    "keys",
                    StemMix {
                        solo: true,
                        ..StemMix::default()
                    },
                );
            }),
        ),
        (
            Command::SetStemSolo {
                stem: KEYS,
                solo: false,
            },
            Box::new(|mix, _| mix.set_stem("keys", StemMix::default())),
        ),
        (
            Command::SetBusVolume {
                bus: Bus::Vocals,
                volume: Volume::from_db(-12.0),
            },
            Box::new(|_, buses| {
                buses.set_level(
                    Bus::Vocals,
                    BusLevel {
                        volume: Volume::from_db(-12.0),
                        mute: false,
                    },
                );
            }),
        ),
        (
            Command::SetBusMute {
                bus: Bus::Guitars,
                mute: true,
            },
            Box::new(|_, buses| {
                buses.set_level(
                    Bus::Guitars,
                    BusLevel {
                        volume: Volume::UNITY,
                        mute: true,
                    },
                );
            }),
        ),
        (
            Command::SetMasterVolume(Volume::from_db(-3.0)),
            Box::new(|_, buses| buses.master_volume = Volume::from_db(-3.0)),
        ),
        // Out of the muted Guitars bus into Vocals, at -12 dB.
        (
            Command::SetStemBus {
                stem: GUITAR,
                bus: Some(Bus::Vocals),
            },
            Box::new(|mix, _| {
                let mut stem = StemMix::default();
                stem.set_bus(Some(Bus::Vocals));
                mix.set_stem("guitar", stem);
            }),
        ),
    ];
    for (command, change) in steps {
        session.send(command).unwrap();
        change(&mut mix, &mut buses);
        let after = gains_of(&mix, &buses);
        let heard = run(&mut session, &fake, RAMP + 512);
        assert_plays(&heard, level(&after));
        let snapshot = session.poll();
        assert_eq!(snapshot.mixer.mix, mix, "{command:?}");
        assert_eq!(snapshot.mixer.buses, buses, "{command:?}");
        assert_eq!(
            snapshot
                .mixer
                .stems
                .iter()
                .map(|stem| stem.gain)
                .collect::<Vec<_>>(),
            after,
            "{command:?}: the gains as they play"
        );
        // No step larger than the ramps of the stems that changed allow.
        let bound: f32 = BAND
            .iter()
            .zip(before.iter().zip(&after))
            .map(|(&(_, _, level), (from, to))| level * (to - from).abs())
            .sum::<f32>()
            / RAMP as f32
            * 1.001
            + 1e-7;
        let joined = [&out[out.len() - 2..], &heard[..]].concat();
        for pair in joined.chunks_exact(2).collect::<Vec<_>>().windows(2) {
            let step = (pair[1][0] - pair[0][0]).abs();
            assert!(step <= bound, "{command:?}: a step of {step} over {bound}");
        }
        out = heard;
        before = after;
    }
    assert_eq!(
        session.poll().mixer.stems[GUITAR].bus,
        Bus::Vocals,
        "the guitar plays through Vocals"
    );
}

#[test]
fn muting_a_bus_silences_every_stem_in_it_and_unmuting_restores_their_levels() {
    let dir = band(None);
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    play(&mut session, &fake, 256);
    session
        .send(Command::SetStemVolume {
            stem: DRUMS,
            volume: Volume::from_db(-6.0),
        })
        .unwrap();
    let mut mix = ArrangementMix::default();
    mix.set_stem("drums", at(-6.0));
    let unmuted = gains_of(&mix, &BusSettings::new());
    assert_plays(&run(&mut session, &fake, 1_024), level(&unmuted));

    session
        .send(Command::SetBusMute {
            bus: Bus::Drums,
            mute: true,
        })
        .unwrap();
    let mut silenced = unmuted.clone();
    silenced[DRUMS] = 0.0;
    silenced[PERCUSSION] = 0.0;
    assert_plays(&run(&mut session, &fake, 1_024), level(&silenced));
    let snapshot = session.poll();
    assert_eq!(snapshot.mixer.stems[DRUMS].gain, 0.0);
    assert_eq!(snapshot.mixer.stems[PERCUSSION].gain, 0.0);

    session
        .send(Command::SetBusMute {
            bus: Bus::Drums,
            mute: false,
        })
        .unwrap();
    assert_plays(&run(&mut session, &fake, 1_024), level(&unmuted));
    assert_eq!(
        session.poll().mixer.stems[DRUMS].gain,
        unmuted[DRUMS],
        "drums back at -6 dB"
    );
}

#[test]
fn solo_never_mutes_click_or_guide() {
    let dir = band(None);
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    play(&mut session, &fake, 256);
    // Keys moved to Click & Guide are not muted by a solo either.
    session
        .send(Command::SetStemBus {
            stem: KEYS,
            bus: Some(Bus::ClickGuide),
        })
        .unwrap();
    session
        .send(Command::SetStemSolo {
            stem: DRUMS,
            solo: true,
        })
        .unwrap();
    let out = run(&mut session, &fake, 1_024);
    assert_plays(&out, BAND[DRUMS].2 + BAND[KEYS].2 + BAND[GUIDE].2);
}

#[test]
fn the_arrangements_mix_comes_from_mix_json_and_its_problems_are_reported() {
    let mut file = mix_file("default", &[("drums", at(-6.0))]);
    let mut live = ArrangementMix::default();
    live.set_stem("bass", muted());
    live.set_stem("tambourine", muted());
    file.set_arrangement("live", live.clone());
    file.set_arrangement(
        "acoustic",
        mix_file("acoustic", &[("keys", muted())]).arrangement("acoustic"),
    );
    let dir = band(Some(&file));
    for (arrangement, mix) in [("default", file.arrangement("default")), ("live", live)] {
        let fake = FakeOutput::new();
        let mut session = open(
            dir.path(),
            &fake,
            SessionOptions {
                arrangement: arrangement.to_owned(),
                ..options()
            },
        );
        let out = play(&mut session, &fake, 512);
        assert_plays(&out, level(&gains_of(&mix, &BusSettings::new())));
        let mixer = session.poll().mixer;
        assert_eq!(mixer.mix, mix, "{arrangement}");
        assert!(!mixer.mix_changed);
        assert_eq!(mixer.mix_error, None);
        // The same problems for every arrangement: the file's.
        assert_eq!(
            mixer.mix_problems,
            read_mix(dir.path(), &library::load_song(dir.path()).unwrap().song).problems
        );
        assert!(
            mixer
                .mix_problems
                .contains(&MixValidationError::UnknownArrangement {
                    arrangement: "acoustic".to_owned()
                }),
            "{:?}",
            mixer.mix_problems
        );
        assert_eq!(mixer.mix_problems.len(), 2, "{:?}", mixer.mix_problems);
    }

    // A file that cannot be used: the default mix plays, and the session
    // says why.
    fs::write(dir.path().join("mix.json"), "{ not json").unwrap();
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    let out = play(&mut session, &fake, 512);
    assert_plays(&out, level(&[1.0; 8]));
    let mixer = session.poll().mixer;
    assert!(mixer.mix_error.is_some(), "{mixer:?}");
    assert_eq!(mixer.mix, ArrangementMix::default());
}

#[test]
fn bus_settings_from_the_options_play_from_the_first_frame() {
    let dir = band(None);
    let fake = FakeOutput::new();
    let mut buses = BusSettings::new();
    buses.set_level(
        Bus::Drums,
        BusLevel {
            volume: Volume::UNITY,
            mute: true,
        },
    );
    buses.master_volume = Volume::from_db(-6.0);
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            bus_settings: buses.clone(),
            ..options()
        },
    );
    session.send(Command::Play).unwrap();
    let expected = level(&gains_of(&ArrangementMix::default(), &buses));
    let mut heard = Vec::new();
    for _ in 0..20 {
        let (out, snapshot) = step(&mut session, &fake);
        if snapshot.output_frame > 0 {
            heard.extend(out);
        }
    }
    // Past the start's declick ramp, every frame plays the settings.
    for frame in heard[2 * 256..].chunks_exact(2) {
        assert!(
            (frame[0] - expected).abs() <= 1e-6,
            "{frame:?} vs {expected}"
        );
    }
    let mixer = session.poll().mixer;
    assert_eq!(mixer.buses, buses);
    assert!(!mixer.buses_changed);
}

#[test]
fn the_mix_and_bus_settings_as_played_are_there_to_save() {
    let file = mix_file("live", &[("bass", muted())]);
    let dir = band(Some(&file));
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    session
        .send(Command::SetStemVolume {
            stem: DRUMS,
            volume: Volume::from_db(-6.0),
        })
        .unwrap();
    session
        .send(Command::SetBusVolume {
            bus: Bus::Bass,
            volume: Volume::from_db(-3.0),
        })
        .unwrap();
    let mixer = session.poll().mixer;
    assert!(mixer.mix_changed);
    assert!(mixer.buses_changed);

    let (folder, to_save) = session.mix_file().expect("a song item has a mix file");
    assert_eq!(
        folder.canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
    assert_eq!(
        to_save.arrangement("default").stem("drums").volume,
        Volume::from_db(-6.0)
    );
    assert_eq!(
        to_save.arrangement("live"),
        file.arrangement("live"),
        "the other arrangements stay as they were"
    );
    write_mix(&folder, &to_save).unwrap();
    session.mix_saved();
    assert_eq!(
        session.bus_settings().level(Bus::Bass).volume,
        Volume::from_db(-3.0)
    );
    session.bus_settings_saved();
    let mixer = session.poll().mixer;
    assert!(!mixer.mix_changed);
    assert!(!mixer.buses_changed);
    let song = library::load_song(dir.path()).unwrap().song;
    assert_eq!(read_mix(dir.path(), &song).mix, to_save);

    // Back to what was saved is no change.
    session
        .send(Command::SetStemVolume {
            stem: DRUMS,
            volume: Volume::UNITY,
        })
        .unwrap();
    assert!(session.poll().mixer.mix_changed);
    session
        .send(Command::SetStemVolume {
            stem: DRUMS,
            volume: Volume::from_db(-6.0),
        })
        .unwrap();
    assert!(!session.poll().mixer.mix_changed);
}

#[test]
fn an_unknown_stem_or_a_volume_out_of_range_is_refused() {
    let dir = band(None);
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    let error = session
        .send(Command::SetStemMute {
            stem: 8,
            mute: true,
        })
        .unwrap_err();
    assert_eq!(error, RuntimeError::UnknownStem { stem: 8, stems: 8 });
    assert_eq!(
        error.to_string(),
        "stem 8 is not in the song, which has 8 stem(s)"
    );
    for db in [f64::NAN, 12.5, f64::INFINITY] {
        let error = session
            .send(Command::SetMasterVolume(Volume::from_db(db)))
            .unwrap_err();
        assert!(
            matches!(error, RuntimeError::InvalidVolume { .. }),
            "{db}: {error:?}"
        );
        assert_eq!(session.poll().last_error, Some(error));
    }
    assert!(
        session
            .send(Command::SetBusVolume {
                bus: Bus::Bass,
                volume: Volume::SILENT,
            })
            .is_ok(),
        "silence is a volume"
    );
    assert_eq!(
        RuntimeError::InvalidVolume {
            db: "12.5".to_owned()
        }
        .to_string(),
        "a volume is silent or at most 12 dB, not 12.5 dB"
    );
    let mixer = session.poll().mixer;
    assert_eq!(mixer.mix, ArrangementMix::default(), "nothing else changed");
    assert_eq!(mixer.buses.master_volume, Volume::UNITY);
}

#[test]
fn each_setlist_item_plays_its_own_mix_and_bus_changes_reach_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    write_band(
        &dir.path().join("first"),
        Some(&mix_file("default", &[("drums", muted())])),
    );
    write_band(
        &dir.path().join("second"),
        Some(&mix_file("default", &[("bass", muted())])),
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
    let mut session =
        SetlistSession::open(&library, &file, Box::new(fake.clone()), options()).unwrap();
    let step = |session: &mut SetlistSession| {
        for monitor in session.disk_monitors() {
            assert!(monitor.wait_idle(Duration::from_secs(10)));
        }
        let out = fake.render(256).expect("a stream plays");
        (out, session.poll())
    };
    let first = mix_file("default", &[("drums", muted())]).arrangement("default");
    let second = mix_file("default", &[("bass", muted())]).arrangement("default");
    let mut buses = BusSettings::new();

    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    // Before the second song is cued: it takes the change when it is.
    session
        .send(SetlistCommand::Item(Command::SetBusMute {
            bus: Bus::Vocals,
            mute: true,
        }))
        .unwrap();
    buses.set_level(
        Bus::Vocals,
        BusLevel {
            volume: Volume::UNITY,
            mute: true,
        },
    );
    let mut out = Vec::new();
    assert!(
        (0..100).any(|_| {
            let (frames, snapshot) = step(&mut session);
            out = frames;
            snapshot.setlist.next_ready
        }),
        "the second song is cued"
    );
    // Once it is cued.
    session
        .send(SetlistCommand::Item(Command::SetMasterVolume(
            Volume::from_db(-6.0),
        )))
        .unwrap();
    buses.master_volume = Volume::from_db(-6.0);
    for _ in 0..4 {
        out = step(&mut session).0;
    }
    assert_plays(&out, level(&gains_of(&first, &buses)));
    assert_eq!(session.poll().song.mixer.mix, first);

    let mut snapshot = session.poll();
    for _ in 0..(FRAMES / 256 + 8) {
        let (frames, next) = step(&mut session);
        out = frames;
        snapshot = next;
        if snapshot.setlist.current == 1 && snapshot.song.output_frame > 1_024 {
            break;
        }
    }
    assert_eq!(snapshot.setlist.current, 1, "the second song plays");
    assert_plays(&out, level(&gains_of(&second, &buses)));
    assert_eq!(snapshot.song.mixer.mix, second);
    assert_eq!(snapshot.song.mixer.buses, buses);
    assert_eq!(
        session
            .mix_file()
            .map(|(folder, _)| folder.ends_with("second")),
        Some(true)
    );
}

#[test]
fn reset_to_song_mix_puts_every_stem_back_at_its_defaults_and_keeps_the_buses() {
    let mut on_vocals = StemMix::default();
    on_vocals.set_bus(Some(Bus::Vocals));
    let soloed = StemMix {
        solo: true,
        ..StemMix::default()
    };
    let saved = mix_file(
        "default",
        &[
            ("drums", at(-6.0)),
            ("bass", muted()),
            ("keys", soloed),
            ("guitar", on_vocals),
        ],
    );
    let dir = band(Some(&saved));
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, options());
    play(&mut session, &fake, 512);
    for command in [
        Command::SetStemVolume {
            stem: PERCUSSION,
            volume: Volume::from_db(-12.0),
        },
        Command::SetBusVolume {
            bus: Bus::Drums,
            volume: Volume::from_db(-6.0),
        },
        Command::SetMasterVolume(Volume::from_db(-3.0)),
    ] {
        session.send(command).unwrap();
    }
    let buses = session.bus_settings().clone();

    session.send(Command::ResetMix).unwrap();

    let mixer = session.poll().mixer;
    assert_eq!(mixer.mix, ArrangementMix::default());
    assert!(mixer.mix_changed, "the saved mix differs");
    assert_eq!(mixer.buses, buses, "bus and master settings stay");
    assert_eq!(
        mixer.stems[GUITAR].bus,
        Bus::Guitars,
        "the rules table's bus"
    );
    let expected = gains_of(&ArrangementMix::default(), &buses);
    let gains: Vec<f32> = mixer.stems.iter().map(|stem| stem.gain).collect();
    assert_eq!(gains, expected);
    let out = run(&mut session, &fake, 4 * RAMP);
    assert_plays(&out, level(&expected));

    // Saved, the arrangement's mix is forgotten.
    let (folder, to_save) = session.mix_file().expect("a song item has a mix file");
    assert_eq!(to_save.arrangement("default"), ArrangementMix::default());
    write_mix(&folder, &to_save).unwrap();
    session.mix_saved();
    assert!(!session.poll().mixer.mix_changed);
}
