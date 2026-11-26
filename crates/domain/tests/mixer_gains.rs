//! S1: global bus settings and the effective gain of every stem (F7.1, F7.3,
//! F7.5, and the F7 acceptance).

use domain::{
    ArrangementMix, BUS_SETTINGS_FORMAT_VERSION, Bus, BusLevel, BusSettings, BusSettingsError,
    MixFile, MixerFile, MixerFileError, Stem, StemGain, StemMix, StemRole, Volume, effective_gains,
};

fn stem(id: &str, part: &str, role: StemRole) -> Stem {
    Stem {
        id: id.to_owned(),
        file: format!("stems/{id}.wav"),
        part: part.to_owned(),
        role,
    }
}

/// One stem per bus or two, plus the click and guide stems.
fn stems() -> Vec<Stem> {
    vec![
        stem("click", "click", StemRole::Click),
        stem("guide", "guide", StemRole::Guide),
        stem("kick", "Kick", StemRole::Track),
        stem("snare", "Snare", StemRole::Track),
        stem("bass", "Bass", StemRole::Track),
        stem("keys", "Keys", StemRole::Track),
        stem("eg", "EG 1", StemRole::Track),
        stem("vox", "Vox", StemRole::Track),
    ]
}

fn db_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

fn gains(mix: &ArrangementMix, buses: &BusSettings) -> Vec<f32> {
    effective_gains(&stems(), mix, buses)
        .into_iter()
        .map(|g| g.gain)
        .collect()
}

fn with_stem(mix: &mut ArrangementMix, id: &str, edit: impl FnOnce(&mut StemMix)) {
    let mut stem = mix.stem(id);
    edit(&mut stem);
    mix.set_stem(id, stem);
}

fn with_bus(buses: &mut BusSettings, bus: Bus, edit: impl FnOnce(&mut BusLevel)) {
    let mut level = buses.level(bus);
    edit(&mut level);
    buses.set_level(bus, level);
}

#[test]
fn unity_everywhere_gives_exactly_one_on_the_rules_table_buses() {
    // A song without mix.json and with default bus settings plays as today.
    let mix = MixFile::new().arrangement("default");
    let result = effective_gains(&stems(), &mix, &BusSettings::new());
    let expected = [
        Bus::ClickGuide,
        Bus::ClickGuide,
        Bus::Drums,
        Bus::Drums,
        Bus::Bass,
        Bus::KeysPads,
        Bus::Guitars,
        Bus::Vocals,
    ]
    .map(|bus| StemGain { bus, gain: 1.0 });
    assert_eq!(result, expected);
    assert!(result.iter().all(|g| g.gain.to_bits() == 1.0f32.to_bits()));
    for bus in Bus::ALL {
        assert_eq!(BusSettings::new().bus_gain(bus).to_bits(), 1.0f32.to_bits());
    }
}

#[test]
fn volume_gains_are_in_db_and_exact_at_the_ends() {
    assert_eq!(Volume::UNITY.gain(), 1.0);
    assert_eq!(Volume::default(), Volume::UNITY);
    assert_eq!(Volume::SILENT.gain(), 0.0);
    assert!((Volume::from_db(-6.0).gain() - 0.501_187).abs() < 1e-6);
    assert!((Volume::from_db(12.0).gain() - 3.981_072).abs() < 1e-6);
    // Invalid volumes never reach the audio as such.
    assert_eq!(Volume::from_db(f64::NAN).gain(), 0.0);
    assert_eq!(
        Volume::from_db(40.0).gain(),
        Volume::from_db(12.0).gain(),
        "capped at +12 dB"
    );
    assert!(Volume::SILENT.is_valid() && Volume::from_db(12.0).is_valid());
    assert!(!Volume::from_db(12.01).is_valid() && !Volume::from_db(f64::NAN).is_valid());
    assert!(!Volume::from_db(f64::INFINITY).is_valid());
}

#[test]
fn stem_bus_and_master_volumes_multiply() {
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "kick", |s| s.volume = Volume::from_db(-6.0));
    let mut buses = BusSettings::new();
    with_bus(&mut buses, Bus::Drums, |b| b.volume = Volume::from_db(-3.0));
    buses.master_volume = Volume::from_db(-1.5);

    let result = gains(&mix, &buses);
    let master = db_gain(-1.5);
    let expected = [
        master,                                 // click: master applies to all
        master,                                 // guide
        db_gain(-6.0) * db_gain(-3.0) * master, // kick
        db_gain(-3.0) * master,                 // snare
        master,
        master,
        master,
        master,
    ];
    for (got, want) in result.iter().zip(expected) {
        assert!((f64::from(*got) - want).abs() < 1e-6, "{got} != {want}");
    }
    assert!((f64::from(buses.bus_gain(Bus::Drums)) - db_gain(-3.0) * master).abs() < 1e-6);
    assert!((f64::from(buses.bus_gain(Bus::ClickGuide)) - master).abs() < 1e-6);
}

#[test]
fn a_muted_or_silent_stem_gives_zero() {
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "bass", |s| s.mute = true);
    with_stem(&mut mix, "keys", |s| s.volume = Volume::SILENT);
    let result = gains(&mix, &BusSettings::new());
    assert_eq!(result, [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0]);
}

#[test]
fn muting_a_bus_silences_every_stem_in_it_and_unmuting_restores_their_levels() {
    // F7 acceptance.
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "kick", |s| s.volume = Volume::from_db(-4.5));
    with_stem(&mut mix, "snare", |s| s.volume = Volume::from_db(2.0));
    let mut buses = BusSettings::new();
    with_bus(&mut buses, Bus::Drums, |b| b.volume = Volume::from_db(-2.0));
    let before = gains(&mix, &buses);

    with_bus(&mut buses, Bus::Drums, |b| b.mute = true);
    let muted = gains(&mix, &buses);
    assert_eq!(muted[2..4], [0.0, 0.0], "kick and snare are silent");
    assert_eq!(muted[..2], before[..2], "other buses keep their levels");
    assert_eq!(muted[4..], before[4..]);
    assert_eq!(buses.bus_gain(Bus::Drums), 0.0);

    with_bus(&mut buses, Bus::Drums, |b| b.mute = false);
    let restored = gains(&mix, &buses);
    assert_eq!(
        restored.map_bits(),
        before.map_bits(),
        "unmute restores the exact levels"
    );
}

#[test]
fn solo_silences_other_stems_but_never_click_or_guide() {
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "eg", |s| s.solo = true);
    let result = gains(&mix, &BusSettings::new());
    assert_eq!(result, [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    with_stem(&mut mix, "vox", |s| s.solo = true);
    let result = gains(&mix, &BusSettings::new());
    assert_eq!(
        result,
        [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0],
        "two solos"
    );
}

#[test]
fn the_solo_exemption_follows_the_click_and_guide_bus_and_roles() {
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "vox", |s| s.solo = true);
    // A track the user sent to Click & Guide is exempt too...
    with_stem(&mut mix, "keys", |s| s.set_bus(Some(Bus::ClickGuide)));
    // ...and a click stem the user sent elsewhere is still the click.
    with_stem(&mut mix, "click", |s| s.set_bus(Some(Bus::Drums)));
    let result = effective_gains(&stems(), &mix, &BusSettings::new());
    assert_eq!(
        result[0],
        StemGain {
            bus: Bus::Drums,
            gain: 1.0
        }
    );
    assert_eq!(
        result[5],
        StemGain {
            bus: Bus::ClickGuide,
            gain: 1.0
        }
    );
    assert_eq!(result[2].gain, 0.0, "kick is not exempt");
}

#[test]
fn a_muted_bus_or_stem_stays_silent_even_when_soloed() {
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "vox", |s| s.solo = true);
    with_stem(&mut mix, "eg", |s| {
        s.solo = true;
        s.mute = true;
    });
    let mut buses = BusSettings::new();
    with_bus(&mut buses, Bus::Vocals, |b| b.mute = true);
    let result = gains(&mix, &buses);
    assert_eq!(result, [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

    // A muted Click & Guide bus silences the click and guide too.
    with_bus(&mut buses, Bus::ClickGuide, |b| b.mute = true);
    assert_eq!(gains(&mix, &buses)[..2], [0.0, 0.0]);
    assert_eq!(buses.bus_gain(Bus::ClickGuide), 0.0);
}

#[test]
fn invalid_volumes_give_finite_gains() {
    let mut mix = ArrangementMix::default();
    with_stem(&mut mix, "kick", |s| s.volume = Volume::from_db(f64::NAN));
    let mut buses = BusSettings::new();
    buses.master_volume = Volume::from_db(f64::INFINITY);
    for gain in gains(&mix, &buses) {
        assert!(gain.is_finite(), "{gain}");
    }
}

const SETTINGS: &str = r#"{
  "formatVersion": 1,
  "buses": {
    "drums": { "volumeDb": -2.0, "mute": false },
    "vocals": { "volumeDb": "-inf", "mute": true }
  },
  "masterVolumeDb": -1.5
}"#;

#[test]
fn bus_settings_round_trip_with_defaults_at_unity() {
    let settings = BusSettings::from_json(SETTINGS).expect("parses");
    assert_eq!(settings.format_version, BUS_SETTINGS_FORMAT_VERSION);
    assert_eq!(
        settings.level(Bus::Drums),
        BusLevel {
            volume: Volume::from_db(-2.0),
            mute: false
        }
    );
    assert_eq!(
        settings.level(Bus::Vocals),
        BusLevel {
            volume: Volume::SILENT,
            mute: true
        }
    );
    assert_eq!(settings.level(Bus::Bass), BusLevel::default());
    assert_eq!(settings.master_volume, Volume::from_db(-1.5));
    assert_eq!(settings.validate(), Ok(()));

    let json = settings.to_json().expect("serializes");
    assert_eq!(BusSettings::from_json(&json).unwrap(), settings);
    let written: serde_json::Value = serde_json::from_str(&json).unwrap();
    let original: serde_json::Value = serde_json::from_str(SETTINGS).unwrap();
    assert_eq!(written, original);

    let defaults = BusSettings::from_json(r#"{ "formatVersion": 1 }"#).expect("parses");
    assert_eq!(defaults, BusSettings::new());
    assert_eq!(defaults, BusSettings::default());
    assert_eq!(defaults.master_volume, Volume::UNITY);
    for bus in Bus::ALL {
        assert_eq!(defaults.level(bus), BusLevel::default());
        assert_eq!(BusLevel::default().volume, Volume::UNITY);
        assert!(!BusLevel::default().mute);
    }

    // Setting a bus back to its default removes its entry.
    let mut settings = settings;
    settings.set_level(Bus::Drums, BusLevel::default());
    assert!(!settings.buses.contains_key(&Bus::Drums));
}

#[test]
fn bus_settings_files_check_their_version_and_shape() {
    let error = BusSettings::from_json(r#"{ "formatVersion": 2, "buses": [] }"#)
        .expect_err("version 2 is refused");
    assert!(matches!(
        error,
        MixerFileError::UnsupportedFormatVersion {
            file: MixerFile::BusSettings,
            found: 2,
            supported: 1
        }
    ));
    assert!(error.to_string().starts_with("bus settings: "), "{error}");

    let error = BusSettings::from_json(r#"{ "formatVersion": 1, "buses": { "brass": {} } }"#)
        .expect_err("unknown bus");
    assert!(
        matches!(
            error,
            MixerFileError::Json {
                file: MixerFile::BusSettings,
                ..
            }
        ),
        "{error:?}"
    );

    let mut settings = BusSettings::new();
    settings.master_volume = Volume::from_db(f64::NAN);
    assert!(matches!(
        settings.to_json(),
        Err(MixerFileError::Json {
            file: MixerFile::BusSettings,
            ..
        })
    ));
}

#[test]
fn bus_settings_validation_reports_every_problem_at_once() {
    let mut settings = BusSettings::new();
    settings.format_version = 7;
    with_bus(&mut settings, Bus::Bass, |b| {
        b.volume = Volume::from_db(13.0)
    });
    with_bus(&mut settings, Bus::Vocals, |b| {
        b.volume = Volume::from_db(20.0)
    });
    settings.master_volume = Volume::from_db(12.5);
    let errors = settings.validate().expect_err("broken rules");
    assert_eq!(
        errors,
        [
            BusSettingsError::UnsupportedFormatVersion { found: 7 },
            BusSettingsError::BusVolumeOutOfRange {
                bus: Bus::Bass,
                db: 13.0
            },
            BusSettingsError::BusVolumeOutOfRange {
                bus: Bus::Vocals,
                db: 20.0
            },
            BusSettingsError::MasterVolumeOutOfRange { db: 12.5 },
        ]
    );
    assert_eq!(
        errors[1].to_string(),
        "bus Bass has volume 13 dB; a volume is \"-inf\" (silent) or a number of dB up to +12"
    );
    assert_eq!(
        errors[3].to_string(),
        "the master volume is 12.5 dB; a volume is \"-inf\" (silent) or a number of dB up to +12"
    );
}

trait MapBits {
    fn map_bits(&self) -> Vec<u32>;
}

impl MapBits for Vec<f32> {
    fn map_bits(&self) -> Vec<u32> {
        self.iter().map(|g| g.to_bits()).collect()
    }
}
