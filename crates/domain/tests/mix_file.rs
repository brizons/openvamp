//! S1: the mix of each arrangement and its `mix.json` file (F7.1, F7.2, F7.5).

mod common;

use common::{spec_package, stem};
use domain::{
    ArrangementMix, Bus, MAX_VOLUME_DB, MIX_FORMAT_VERSION, MixFile, MixValidationError, MixerFile,
    MixerFileError, StemMix, StemRole, Volume,
};

/// A `mix.json` exercising every field of format v1.
const EXAMPLE: &str = r#"{
  "formatVersion": 1,
  "arrangements": {
    "default": {
      "stems": {
        "drums": { "volumeDb": -3.5, "mute": false, "solo": true, "bus": "bass" },
        "click": { "volumeDb": "-inf", "mute": true, "solo": false }
      }
    }
  }
}"#;

fn example() -> MixFile {
    let mut default = ArrangementMix::default();
    default.stems.insert(
        "drums".to_owned(),
        StemMix {
            volume: Volume::from_db(-3.5),
            mute: false,
            solo: true,
            bus: Some("bass".to_owned()),
        },
    );
    default.stems.insert(
        "click".to_owned(),
        StemMix {
            volume: Volume::SILENT,
            mute: true,
            solo: false,
            bus: None,
        },
    );
    let mut file = MixFile::new();
    file.arrangements.insert("default".to_owned(), default);
    file
}

fn json_value(json: &str) -> serde_json::Value {
    serde_json::from_str(json).expect("valid JSON")
}

#[test]
fn example_parses_round_trips_and_validates() {
    let file = MixFile::from_json(EXAMPLE).expect("example parses");
    assert_eq!(file, example());
    assert_eq!(file.format_version, MIX_FORMAT_VERSION);

    let json = file.to_json().expect("serializes");
    assert_eq!(MixFile::from_json(&json).expect("reparses"), file);
    assert_eq!(
        json_value(&json),
        json_value(EXAMPLE),
        "the written file keeps every field, silence as \"-inf\""
    );

    assert_eq!(file.validate(&spec_package()), Ok(()));
}

#[test]
fn missing_fields_and_entries_mean_defaults() {
    let file = MixFile::from_json(r#"{ "formatVersion": 1 }"#).expect("parses");
    assert!(file.arrangements.is_empty());
    assert_eq!(file, MixFile::new());

    let file = MixFile::from_json(
        r#"{ "formatVersion": 1, "arrangements": { "default": { "stems": { "drums": {} } } } }"#,
    )
    .expect("parses");
    let drums = file.arrangement("default").stem("drums");
    assert_eq!(drums, StemMix::default());
    assert_eq!(drums.volume, Volume::UNITY, "unity");
    assert!(!drums.mute && !drums.solo && drums.bus.is_none());

    // No entry at all: unity, unmuted, unsoloed, the rules-table bus.
    let empty = MixFile::new();
    let mix = empty.arrangement("acoustic");
    assert_eq!(mix, ArrangementMix::default());
    assert_eq!(mix.stem("drums"), StemMix::default());
    assert_eq!(mix.bus_of(&stem("drums", StemRole::Track)), Bus::Drums);
    assert_eq!(mix.bus_of(&stem("click", StemRole::Click)), Bus::ClickGuide);
    assert_eq!(mix.bus_of(&stem("mystery", StemRole::Track)), Bus::KeysPads);
}

#[test]
fn a_bus_override_replaces_the_rules_table_bus() {
    let file = example();
    let mix = file.arrangement("default");
    assert_eq!(mix.stem("drums").bus_override(), Some(Bus::Bass));
    assert_eq!(mix.bus_of(&stem("drums", StemRole::Track)), Bus::Bass);

    let mut drums = mix.stem("drums");
    drums.set_bus(Some(Bus::Vocals));
    assert_eq!(drums.bus.as_deref(), Some("vocals"));
    drums.set_bus(None);
    assert_eq!(drums.bus, None);

    // An unknown bus id (rejected by `validate`) leaves the rules-table bus.
    let unknown = StemMix {
        bus: Some("brass".to_owned()),
        ..StemMix::default()
    };
    assert_eq!(unknown.bus_override(), None);
    let mut mix = ArrangementMix::default();
    mix.set_stem("drums", unknown);
    assert_eq!(mix.bus_of(&stem("drums", StemRole::Track)), Bus::Drums);
}

#[test]
fn setting_defaults_removes_entries_so_the_file_stays_sparse() {
    let mut file = example();
    let mut mix = file.arrangement("default");
    mix.set_stem("click", StemMix::default());
    assert!(!mix.stems.contains_key("click"));
    mix.set_stem(
        "drums",
        StemMix {
            mute: true,
            ..StemMix::default()
        },
    );
    assert!(mix.stems["drums"].mute);
    file.set_arrangement("default", mix);
    assert!(file.arrangements["default"].stems.contains_key("drums"));

    // "Reset to song mix": a default mix removes the arrangement's entry.
    file.set_arrangement("default", ArrangementMix::default());
    assert!(file.arrangements.is_empty());
    assert_eq!(
        json_value(&file.to_json().unwrap()),
        json_value(r#"{ "formatVersion": 1, "arrangements": {} }"#)
    );
}

#[test]
fn a_newer_format_version_is_reported_before_the_shape() {
    let error = MixFile::from_json(r#"{ "formatVersion": 2, "arrangements": 5 }"#)
        .expect_err("version 2 is refused");
    assert!(
        matches!(
            error,
            MixerFileError::UnsupportedFormatVersion {
                file: MixerFile::Mix,
                found: 2,
                supported: 1
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "mix.json: unsupported formatVersion 2; this version reads formatVersion 1"
    );
}

#[test]
fn a_wrong_shape_is_a_json_error() {
    for json in [
        "not json",
        r#"{ "arrangements": {} }"#,
        r#"{ "formatVersion": 1, "arrangements": { "default": { "stems": { "drums": { "volumeDb": "loud" } } } } }"#,
        r#"{ "formatVersion": 1, "arrangements": { "default": { "stems": { "drums": { "volumeDb": "+inf" } } } } }"#,
        r#"{ "formatVersion": 1, "arrangements": { "default": { "stems": { "drums": { "mute": "yes" } } } } }"#,
    ] {
        let error = MixFile::from_json(json).expect_err("malformed");
        assert!(
            matches!(
                error,
                MixerFileError::Json {
                    file: MixerFile::Mix,
                    ..
                }
            ),
            "{json}: {error:?}"
        );
        assert!(error.to_string().starts_with("mix.json: "), "{error}");
    }
}

#[test]
fn a_volume_json_cannot_hold_is_not_written() {
    for db in [f64::NAN, f64::INFINITY] {
        let mut mix = ArrangementMix::default();
        mix.set_stem(
            "drums",
            StemMix {
                volume: Volume::from_db(db),
                ..StemMix::default()
            },
        );
        let mut file = MixFile::new();
        file.set_arrangement("default", mix);
        let error = file.to_json().expect_err("not representable");
        assert!(
            matches!(
                error,
                MixerFileError::Json {
                    file: MixerFile::Mix,
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn validation_reports_every_problem_at_once() {
    let mut file = MixFile::from_json(
        r#"{
  "formatVersion": 1,
  "arrangements": {
    "default": {
      "stems": {
        "drums": { "volumeDb": 12.5, "bus": "brass" },
        "click": { "volumeDb": 12.0 },
        "piano": { "mute": true }
      }
    },
    "acoustic": {
      "stems": { "drums": { "volumeDb": "-inf", "bus": "drums" } }
    }
  }
}"#,
    )
    .expect("parses");
    file.format_version = 3;
    let mut tail = ArrangementMix::default();
    tail.set_stem(
        "click",
        StemMix {
            volume: Volume::from_db(f64::NAN),
            ..StemMix::default()
        },
    );
    file.set_arrangement("tail", tail);

    let errors = file
        .validate(&spec_package())
        .expect_err("the file breaks rules");
    assert_eq!(errors.len(), 7, "{errors:#?}");
    assert_eq!(
        errors[..6],
        [
            MixValidationError::UnsupportedFormatVersion { found: 3 },
            MixValidationError::UnknownArrangement {
                arrangement: "acoustic".to_owned()
            },
            MixValidationError::VolumeOutOfRange {
                arrangement: "default".to_owned(),
                stem: "drums".to_owned(),
                db: 12.5
            },
            MixValidationError::UnknownBus {
                arrangement: "default".to_owned(),
                stem: "drums".to_owned(),
                bus: "brass".to_owned()
            },
            MixValidationError::UnknownStem {
                arrangement: "default".to_owned(),
                stem: "piano".to_owned()
            },
            MixValidationError::UnknownArrangement {
                arrangement: "tail".to_owned()
            },
        ],
        "every problem, in file order; +{MAX_VOLUME_DB} dB itself is accepted"
    );
    match &errors[6] {
        MixValidationError::VolumeOutOfRange {
            arrangement,
            stem,
            db,
        } => assert!(arrangement == "tail" && stem == "click" && db.is_nan()),
        other => panic!("expected the NaN volume, got {other:?}"),
    }

    let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages[..6],
        [
            "unsupported formatVersion 3; this version reads formatVersion 1",
            "mix.json has a mix for arrangement \"acoustic\", which the song does not have",
            "arrangement \"default\" stem \"drums\" has volume 12.5 dB; a volume is \"-inf\" (silent) or a number of dB up to +12",
            "arrangement \"default\" sends stem \"drums\" to unknown bus \"brass\"; the buses are clickGuide, drums, bass, keysPads, guitars, vocals",
            "arrangement \"default\" mixes stem \"piano\", which the song does not have",
            "mix.json has a mix for arrangement \"tail\", which the song does not have",
        ]
    );
}
