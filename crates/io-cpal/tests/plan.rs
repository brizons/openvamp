//! S8 without hardware: `check` refuses exactly what `open` refuses before
//! it builds a stream, because both go through `plan`; what cannot be read
//! (the device list, a busy device's configurations) makes `open` fail and
//! lets `check` go ahead.

use std::cell::Cell;

use app::{DeviceChoice, OutputCheck, OutputError, OutputRequest};
use cpal::{SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};
use io_cpal::{
    Candidate, Plan, Probe, StreamChoice, Unplanned, candidates, check_outcome, choose_config,
    open_outcome, plan,
};

fn stereo(rates: (u32, u32)) -> SupportedStreamConfigRange {
    SupportedStreamConfigRange::new(
        2,
        rates.0,
        rates.1,
        SupportedBufferSize::Range {
            min: 32,
            max: 4_096,
        },
        SampleFormat::F32,
    )
}

/// Two devices, "Speakers" the default; `"busy"` names one whose
/// configurations cannot be read.
fn found() -> Result<Vec<(Candidate, &'static str)>, OutputError> {
    let probe = |id: &str, name: &str| Probe {
        id: Some(id.to_owned()),
        name: Some(name.to_owned()),
    };
    Ok(candidates(
        [
            (probe("host:speakers", "Speakers"), "speakers"),
            (probe("host:mono", "Mono"), "mono"),
            (probe("host:busy", "Busy"), "busy"),
        ],
        Some((probe("host:speakers", "Speakers"), "speakers")),
    ))
}

fn configs(device: &&str) -> Result<Vec<SupportedStreamConfigRange>, String> {
    match *device {
        "speakers" => Ok(vec![stereo((44_100, 48_000))]),
        "mono" => Ok(vec![SupportedStreamConfigRange::new(
            1,
            8_000,
            192_000,
            SupportedBufferSize::Unknown,
            SampleFormat::F32,
        )]),
        _ => Err("the device is busy".to_owned()),
    }
}

fn request(device: Option<&str>, sample_rate: u32) -> OutputRequest {
    OutputRequest {
        device: device.map(|name| DeviceChoice::Named(name.to_owned())),
        sample_rate,
        buffer_frames: 256,
        channels: 2,
    }
}

/// What `check` says and what `open` does before it builds, for one input.
fn both(
    found: Result<Vec<(Candidate, &'static str)>, OutputError>,
    request: &OutputRequest,
) -> (
    OutputCheck,
    Result<(&'static str, StreamChoice), OutputError>,
) {
    let for_check = plan(found.clone(), request, configs);
    let for_open = plan(found, request, configs);
    (
        check_outcome(&for_check),
        open_outcome(for_open).map(|plan| (plan.device, plan.choice)),
    )
}

#[test]
fn check_refuses_exactly_what_open_refuses_before_it_builds() {
    let host = Err(OutputError::Host {
        message: "the audio system is not running".to_owned(),
    });
    let inputs = [
        ("fits", found(), request(None, 48_000)),
        ("rate", found(), request(None, 96_000)),
        ("mono", found(), request(Some("Mono"), 48_000)),
        ("unknown", found(), request(Some("Elsewhere"), 48_000)),
        ("no default", Ok(Vec::new()), request(None, 48_000)),
        ("busy", found(), request(Some("Busy"), 48_000)),
        ("host", host, request(None, 48_000)),
    ];
    for (case, found, request) in inputs {
        let (check, open) = both(found, &request);
        match (&check, &open) {
            (OutputCheck::Fits, Ok(_)) => {}
            (OutputCheck::Refused(refused), Err(error)) => assert_eq!(refused, error, "{case}"),
            (OutputCheck::Unknown(reason), Err(error)) => {
                assert_eq!(reason, &error.to_string(), "{case}");
            }
            _ => panic!("{case}: check {check:?} but open {open:?}"),
        }
    }
}

#[test]
fn a_device_that_cannot_play_the_request_is_refused_with_opens_reason() {
    let (check, open) = both(found(), &request(None, 96_000));
    let expected =
        choose_config("Speakers", &[stereo((44_100, 48_000))], 96_000, 256, 2).unwrap_err();
    assert_eq!(check, OutputCheck::Refused(expected.clone()));
    assert_eq!(open, Err(expected));

    let (check, _) = both(found(), &request(Some("Mono"), 48_000));
    assert!(matches!(
        check,
        OutputCheck::Refused(OutputError::TooFewChannels {
            channels: 1,
            needed: 2,
            ..
        })
    ));
    assert_eq!(
        both(Ok(Vec::new()), &request(None, 48_000)).0,
        OutputCheck::Refused(OutputError::NoDevice)
    );
    assert!(matches!(
        both(found(), &request(Some("Elsewhere"), 48_000)).0,
        OutputCheck::Refused(OutputError::DeviceNotFound { .. })
    ));
}

#[test]
fn configurations_that_cannot_be_read_fail_open_but_let_check_go_ahead() {
    // On ALSA, reading a device's configurations opens its PCM for a moment,
    // which fails while another stream holds it.
    let (check, open) = both(found(), &request(Some("Busy"), 48_000));
    assert_eq!(
        open,
        Err(OutputError::Device {
            device: "Busy".to_owned(),
            message: "the device is busy".to_owned(),
        })
    );
    assert!(matches!(&check, OutputCheck::Unknown(_)), "{check:?}");
    assert!(check.goes_ahead());

    let host = Err(OutputError::Host {
        message: "the audio system is not running".to_owned(),
    });
    assert!(both(host, &request(None, 48_000)).0.goes_ahead());
}

#[test]
fn a_fitting_request_plans_the_device_and_configuration_open_builds() {
    let plan = open_outcome(plan(found(), &request(None, 48_000), configs)).unwrap();
    let Plan {
        candidate,
        device,
        choice,
    } = plan;
    assert_eq!(candidate.name, "Speakers");
    assert!(candidate.is_default);
    assert_eq!(device, "speakers");
    assert_eq!(
        choice,
        StreamChoice {
            channels: 2,
            format: SampleFormat::F32
        }
    );
}

#[test]
fn configurations_are_read_only_for_the_device_the_request_means() {
    let reads = Cell::new(0);
    let counting = |device: &&str| {
        reads.set(reads.get() + 1);
        configs(device)
    };
    let refused = plan(found(), &request(Some("Elsewhere"), 48_000), counting);
    assert!(matches!(refused, Err(Unplanned::Refused(_))));
    assert_eq!(reads.get(), 0, "no device was asked");
    let counting = |device: &&str| {
        reads.set(reads.get() + 1);
        configs(device)
    };
    assert!(plan(found(), &request(None, 48_000), counting).is_ok());
    assert_eq!(reads.get(), 1);
}
