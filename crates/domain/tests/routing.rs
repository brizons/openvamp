//! S2: routing buses to a device's outputs, presets by output count, and split
//! mode (F8.2, F8.3, F8.4).

use std::collections::BTreeMap;

use domain::{
    Bus, Destination, MAX_OUTPUTS, MixerFile, MixerFileError, ROUTING_FORMAT_VERSION, RouteTarget,
    Routing, RoutingError, RoutingFile, RoutingValidationError,
};

use Destination::{Mono, Stereo};

const MAIN: Destination = Stereo { left: 1, right: 2 };

fn buses(destinations: [Destination; 6]) -> BTreeMap<Bus, Destination> {
    Bus::ALL.into_iter().zip(destinations).collect()
}

fn pair(left: u16, right: u16) -> Destination {
    Stereo { left, right }
}

#[track_caller]
fn assert_preset(outputs: u16, split_mode: bool, destinations: [Destination; 6]) {
    let preset = Routing::preset(outputs);
    assert_eq!(
        preset,
        Routing {
            outputs,
            split_mode,
            buses: buses(destinations),
            master: None,
        },
        "preset for {outputs} outputs"
    );
    assert_eq!(preset.validate(), Ok(()), "preset for {outputs} outputs");
}

#[test]
fn presets_follow_the_output_count() {
    // In mixer order: Click & Guide, Drums, Bass, Keys & Pads, Guitars, Vocals.
    assert_preset(1, false, [Mono(1); 6]);
    // Plain stereo, as the user asked (L22); split mode is a switch.
    assert_preset(2, false, [MAIN; 6]);
    assert_eq!(Routing::preset(2), Routing::stereo(2));
    assert_preset(
        3,
        false,
        [
            Mono(1),
            pair(2, 3),
            pair(2, 3),
            pair(2, 3),
            pair(2, 3),
            pair(2, 3),
        ],
    );
    for outputs in [4, 5] {
        assert_preset(
            outputs,
            false,
            [
                MAIN,
                pair(3, 4),
                pair(3, 4),
                pair(3, 4),
                pair(3, 4),
                pair(3, 4),
            ],
        );
    }
    for outputs in [6, 7] {
        assert_preset(
            outputs,
            false,
            [Mono(1), Mono(2), Mono(3), Mono(4), Mono(5), Mono(6)],
        );
    }
    for outputs in [8, 16, 24, 32] {
        assert_preset(
            outputs,
            false,
            [Mono(1), Mono(2), Mono(3), pair(5, 6), Mono(4), pair(7, 8)],
        );
    }
}

#[test]
fn presets_clamp_the_output_count_to_what_a_routing_holds() {
    assert_eq!(Routing::preset(0), Routing::preset(1));
    assert_eq!(MAX_OUTPUTS, 32);
    assert_eq!(Routing::preset(64), Routing::preset(32));
}

#[test]
fn split_mode_sends_click_and_guide_left_and_the_stems_right() {
    // F8.4: "click and guide on the left, the stems summed to mono on the right."
    let mut routing = Routing::preset(2);
    routing.split_mode = true;
    assert_eq!(routing.destination(Bus::ClickGuide), Mono(1));
    for bus in &Bus::ALL[1..] {
        assert_eq!(routing.destination(*bus), Mono(2), "{bus}");
    }
    routing.master = Some(MAIN);
    assert_eq!(routing.master_destination(), None, "only outputs 1 and 2");

    // Turning split mode off plays the stored routing again: plain stereo.
    routing.split_mode = false;
    for bus in Bus::ALL {
        assert_eq!(routing.destination(bus), MAIN, "{bus}");
    }
    assert_eq!(routing.master_destination(), Some(MAIN));

    // Split mode works the same on a larger device.
    let mut routing = Routing::preset(8);
    routing.split_mode = true;
    assert_eq!(routing.destination(Bus::ClickGuide), Mono(1));
    assert_eq!(routing.destination(Bus::Vocals), Mono(2));
}

#[test]
fn the_stereo_routing_plays_every_bus_on_outputs_1_and_2() {
    for outputs in [2, 8, 32] {
        let routing = Routing::stereo(outputs);
        assert_eq!(
            routing,
            Routing {
                outputs,
                split_mode: false,
                buses: buses([MAIN; 6]),
                master: None
            }
        );
        assert_eq!(routing.validate(), Ok(()));
    }
    assert_eq!(Routing::stereo(1).buses, buses([Mono(1); 6]));
}

#[test]
fn a_bus_without_a_destination_plays_on_outputs_1_and_2() {
    let mut routing = Routing::preset(8);
    routing.buses.remove(&Bus::Vocals);
    assert_eq!(routing.destination(Bus::Vocals), MAIN);
    assert_eq!(routing.validate(), Ok(()));
    let mut routing = Routing::preset(1);
    routing.buses.clear();
    assert_eq!(routing.destination(Bus::Drums), Mono(1));
}

#[test]
fn fitting_to_fewer_outputs_falls_back_to_outputs_1_and_2() {
    let mut routing = Routing::preset(8);
    routing.master = Some(pair(7, 8));
    let fitted = routing.fit(4);
    assert_eq!(
        fitted,
        Routing {
            outputs: 4,
            split_mode: false,
            buses: buses([Mono(1), Mono(2), Mono(3), MAIN, Mono(4), MAIN]),
            master: None,
        }
    );
    assert_eq!(fitted.validate(), Ok(()));

    let mut routing = Routing::preset(2);
    routing.master = Some(Mono(1));
    let single = routing.fit(1);
    assert_eq!(
        single,
        Routing {
            outputs: 1,
            split_mode: false,
            buses: buses([Mono(1); 6]),
            master: Some(Mono(1)),
        }
    );
    assert_eq!(single.validate(), Ok(()));

    // Fitting also repairs what validation rejects, and never widens a
    // routing beyond what the device has.
    let mut broken = Routing::preset(8);
    broken.buses.insert(Bus::Bass, pair(3, 3));
    broken.buses.insert(Bus::Drums, Mono(0));
    let repaired = broken.fit(8);
    assert_eq!(repaired.buses[&Bus::Bass], MAIN);
    assert_eq!(repaired.buses[&Bus::Drums], MAIN);
    assert_eq!(repaired.validate(), Ok(()));
    assert_eq!(Routing::preset(8).fit(8), Routing::preset(8));
    assert_eq!(Routing::preset(8).fit(64).outputs, 32);
}

#[test]
fn validation_reports_every_problem_at_once() {
    let mut routing = Routing::preset(8);
    routing.buses.insert(Bus::Drums, Mono(9));
    routing.buses.insert(Bus::Bass, Mono(0));
    routing.buses.insert(Bus::Vocals, pair(4, 4));
    routing.buses.insert(Bus::Guitars, pair(8, 12));
    routing.master = Some(Mono(40));
    assert_eq!(
        routing.validate(),
        Err(vec![
            RoutingError::OutputOutOfRange {
                target: RouteTarget::Bus(Bus::Drums),
                output: 9,
                outputs: 8
            },
            RoutingError::OutputOutOfRange {
                target: RouteTarget::Bus(Bus::Bass),
                output: 0,
                outputs: 8
            },
            RoutingError::OutputOutOfRange {
                target: RouteTarget::Bus(Bus::Guitars),
                output: 12,
                outputs: 8
            },
            RoutingError::SameOutputPair {
                target: RouteTarget::Bus(Bus::Vocals),
                output: 4
            },
            RoutingError::OutputOutOfRange {
                target: RouteTarget::Master,
                output: 40,
                outputs: 8
            },
        ])
    );

    let mut routing = Routing::preset(2);
    routing.split_mode = true;
    routing.outputs = 1;
    let errors = routing.validate().expect_err("split mode on one output");
    assert_eq!(
        errors[0],
        RoutingError::SplitModeNeedsTwoOutputs { outputs: 1 }
    );
    assert_eq!(errors.len(), 1 + 6, "and every bus on output 2: {errors:?}");

    for outputs in [0, 33] {
        let routing = Routing {
            outputs,
            ..Routing::stereo(2)
        };
        let errors = routing.validate().expect_err("output count");
        assert_eq!(errors[0], RoutingError::OutputCountOutOfRange { outputs });
    }

    let messages: Vec<String> = [
        RoutingError::OutputCountOutOfRange { outputs: 33 },
        RoutingError::SplitModeNeedsTwoOutputs { outputs: 1 },
        RoutingError::OutputOutOfRange {
            target: RouteTarget::Bus(Bus::KeysPads),
            output: 9,
            outputs: 8,
        },
        RoutingError::SameOutputPair {
            target: RouteTarget::Master,
            output: 4,
        },
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        messages,
        [
            "the routing has 33 outputs; a device has 1 to 32 outputs",
            "split mode needs 2 outputs, but the routing has 1",
            "bus Keys & Pads goes to output 9, but the device has outputs 1 to 8",
            "the master goes to the stereo pair 4/4; a pair needs two different outputs",
        ]
    );
}

/// A routing file exercising every field of format v1.
const EXAMPLE: &str = r#"{
  "formatVersion": 1,
  "devices": {
    "Built-in Output": {
      "outputs": 2,
      "splitMode": true,
      "buses": {
        "clickGuide": [1, 2],
        "drums": [1, 2],
        "bass": [1, 2],
        "keysPads": [1, 2],
        "guitars": [1, 2],
        "vocals": [1, 2]
      }
    },
    "USB interface": {
      "outputs": 8,
      "splitMode": false,
      "buses": {
        "clickGuide": [1],
        "drums": [2],
        "bass": [3],
        "keysPads": [5, 6],
        "guitars": [4],
        "vocals": [7, 8]
      },
      "master": [7, 8]
    }
  }
}"#;

#[test]
fn routing_files_round_trip_every_device() {
    let file = RoutingFile::from_json(EXAMPLE).expect("example parses");
    assert_eq!(file.format_version, ROUTING_FORMAT_VERSION);
    assert_eq!(
        file.devices["Built-in Output"],
        Routing {
            split_mode: true,
            ..Routing::preset(2)
        }
    );
    let usb = Routing {
        master: Some(pair(7, 8)),
        ..Routing::preset(8)
    };
    assert_eq!(file.devices["USB interface"], usb);
    assert_eq!(file.validate(), Ok(()));

    let json = file.to_json().expect("serializes");
    assert_eq!(RoutingFile::from_json(&json).unwrap(), file);
    let written: serde_json::Value = serde_json::from_str(&json).unwrap();
    let original: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
    assert_eq!(written, original);

    let empty = RoutingFile::from_json(r#"{ "formatVersion": 1 }"#).unwrap();
    assert_eq!(empty, RoutingFile::new());
    assert_eq!(empty, RoutingFile::default());
    let minimal: Routing = serde_json::from_str(r#"{ "outputs": 4 }"#).unwrap();
    assert_eq!(
        minimal,
        Routing {
            outputs: 4,
            split_mode: false,
            buses: BTreeMap::new(),
            master: None
        }
    );
}

#[test]
fn routing_files_check_their_version_and_shape() {
    let error =
        RoutingFile::from_json(r#"{ "formatVersion": 2, "devices": 1 }"#).expect_err("version 2");
    assert!(matches!(
        error,
        MixerFileError::UnsupportedFormatVersion {
            file: MixerFile::Routing,
            found: 2,
            supported: 1
        }
    ));
    for destination in ["[]", "[1, 2, 3]", "\"1/2\"", "[-1]", "{ \"mono\": 1 }"] {
        let json = format!(
            r#"{{ "formatVersion": 1, "devices": {{ "d": {{ "outputs": 8, "buses": {{ "drums": {destination} }} }} }} }}"#
        );
        let error = RoutingFile::from_json(&json).expect_err(destination);
        assert!(
            matches!(
                error,
                MixerFileError::Json {
                    file: MixerFile::Routing,
                    ..
                }
            ),
            "{destination}: {error:?}"
        );
        assert!(error.to_string().starts_with("routing: "), "{error}");
    }
}

#[test]
fn a_device_plays_its_saved_routing_or_the_preset_for_its_outputs() {
    let mut file = RoutingFile::new();
    assert_eq!(file.routing("USB interface", 8), Routing::preset(8));
    assert_eq!(file.routing("Built-in Output", 2), Routing::preset(2));

    let mut edited = Routing::preset(8);
    edited.buses.insert(Bus::Vocals, Mono(8));
    file.set_routing("USB interface", edited.clone());
    assert_eq!(file.routing("USB interface", 8), edited, "saved per device");
    assert_eq!(
        file.routing("USB interface", 4),
        edited.fit(4),
        "fitted when the device reports fewer outputs"
    );
    assert_eq!(file.routing("Another device", 8), Routing::preset(8));
}

#[test]
fn routing_file_validation_names_each_device() {
    let mut file = RoutingFile::from_json(EXAMPLE).unwrap();
    file.format_version = 4;
    let mut bad = Routing::preset(8);
    bad.buses.insert(Bus::Drums, Mono(9));
    file.set_routing("USB interface", bad);
    let errors = file.validate().expect_err("broken");
    assert_eq!(
        errors,
        [
            RoutingValidationError::UnsupportedFormatVersion { found: 4 },
            RoutingValidationError::Device {
                device: "USB interface".to_owned(),
                error: RoutingError::OutputOutOfRange {
                    target: RouteTarget::Bus(Bus::Drums),
                    output: 9,
                    outputs: 8
                }
            },
        ]
    );
    assert_eq!(
        errors[1].to_string(),
        "device \"USB interface\": bus Drums goes to output 9, but the device has outputs 1 to 8"
    );
}

#[test]
fn a_pair_on_one_missing_output_reports_the_output_once_and_the_pair() {
    let mut routing = Routing::preset(8);
    routing.buses.insert(Bus::Drums, pair(9, 9));
    assert_eq!(
        routing.validate(),
        Err(vec![
            RoutingError::OutputOutOfRange {
                target: RouteTarget::Bus(Bus::Drums),
                output: 9,
                outputs: 8
            },
            RoutingError::SameOutputPair {
                target: RouteTarget::Bus(Bus::Drums),
                output: 9
            },
        ])
    );
    assert!(!pair(9, 9).fits(8) && !pair(3, 3).fits(8) && !Mono(0).fits(8));
    assert!(pair(1, 8).fits(8) && Mono(8).fits(8));
}
