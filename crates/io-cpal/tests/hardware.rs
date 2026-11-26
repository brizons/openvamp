//! S4 on real hardware. These tests are ignored by default because they need
//! an audio output device; run them with
//!
//! ```text
//! cargo test -p io-cpal --test hardware -- --ignored --test-threads=1
//! ```
//!
//! They open the default output for a moment and never start the transport,
//! so the device only receives silence. Unplugging and plugging the device
//! back in is checked by hand with `play-cli` (see its README).

use std::fs;
use std::path::Path;

use app::{
    AudioOutput, DeviceStatus, OutputCheck, OutputError, OutputRequest, Session, SessionOptions,
    TransportState,
};
use domain::{
    Arrangement, SectionMarker, SectionType, SongPackage, Stem, StemRole, TempoPoint,
    TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use io_cpal::CpalOutput;

const RATE: u32 = 48_000;

/// One silent second at 48 kHz, the rate most devices run at.
fn write_silent_song(dir: &Path) {
    let package = SongPackage {
        format_version: 1,
        title: "Hardware Test".to_owned(),
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
            id: "pad".to_owned(),
            file: "stems/pad.wav".to_owned(),
            part: "pad".to_owned(),
            role: StemRole::Track,
        }],
        sections: vec![SectionMarker {
            id: "intro".to_owned(),
            section_type: SectionType::Intro,
            number: None,
            start: 0,
        }],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["intro".to_owned()],
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
    let mut writer = WavWriter::create(dir.join("stems/pad.wav"), spec).unwrap();
    for _ in 0..RATE {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();
}

#[test]
#[ignore = "needs an audio output device"]
fn lists_the_output_devices_with_their_channel_counts() {
    let devices = CpalOutput::new().devices().expect("the audio host answers");
    assert!(!devices.is_empty(), "this machine has an output device");
    assert!(devices.iter().all(|device| !device.name.is_empty()));
    let mut ids: Vec<&str> = devices.iter().map(|device| device.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), devices.len(), "no two devices share an id");
    assert!(devices.iter().filter(|device| device.is_default).count() <= 1);
    assert!(devices.iter().any(|device| device.output_channels >= 2));
}

#[test]
#[ignore = "needs an audio output device that runs at 48 kHz"]
fn opens_the_default_device_at_the_songs_rate_and_reports_the_buffer_latency() {
    let dir = tempfile::tempdir().unwrap();
    write_silent_song(dir.path());
    let mut session = Session::open(
        dir.path(),
        Box::new(CpalOutput::new()),
        SessionOptions {
            buffer_frames: 256,
            ..SessionOptions::default()
        },
    )
    .expect("the default device opens at 48 kHz");

    let snapshot = session.poll();
    let DeviceStatus::Connected(info) = snapshot.device else {
        panic!("the device plays");
    };
    assert_eq!(info.sample_rate, RATE);
    let default = session
        .devices()
        .expect("the audio host answers")
        .into_iter()
        .find(|device| device.is_default)
        .expect("a default output");
    assert_eq!(
        info.device_id, default.id,
        "the default device, pinned by id"
    );
    assert!(info.channels >= 2);
    assert!((64..=1_024).contains(&info.buffer_frames));
    assert_eq!(
        info.latency().as_secs_f64(),
        f64::from(info.buffer_frames) / f64::from(RATE)
    );
    assert_eq!(snapshot.transport, TransportState::Stopped);
}

#[test]
#[ignore = "needs an audio output device that runs at 48 kHz"]
fn checks_the_default_device_at_the_songs_rate_without_a_stream() {
    let output = CpalOutput::new();
    let request = |sample_rate| OutputRequest {
        device: None,
        sample_rate,
        buffer_frames: 256,
        channels: 2,
    };
    assert_eq!(output.check(&request(RATE)), OutputCheck::Fits);
    assert!(matches!(
        output.check(&request(1)),
        OutputCheck::Refused(OutputError::UnsupportedSampleRate { requested: 1, .. })
    ));
}
