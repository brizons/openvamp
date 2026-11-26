//! S4 without hardware: which stream configuration a device gets, how the
//! session's channels reach the device's, and what a stream error means for
//! the session.

use app::{LossReason, OutputError};
use cpal::{ErrorKind, SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};
use io_cpal::{StreamChoice, choose_config, loss_reason, write_interleaved};

fn range(
    channels: u16,
    rates: (u32, u32),
    buffer: Option<(u32, u32)>,
    format: SampleFormat,
) -> SupportedStreamConfigRange {
    let buffer_size = match buffer {
        Some((min, max)) => SupportedBufferSize::Range { min, max },
        None => SupportedBufferSize::Unknown,
    };
    SupportedStreamConfigRange::new(channels, rates.0, rates.1, buffer_size, format)
}

#[test]
fn the_choice_prefers_float_and_the_fewest_channels_at_the_songs_rate() {
    let ranges = [
        range(8, (44_100, 96_000), Some((32, 4_096)), SampleFormat::F32),
        range(2, (44_100, 96_000), Some((32, 4_096)), SampleFormat::I16),
        range(2, (44_100, 96_000), Some((32, 4_096)), SampleFormat::F32),
        range(1, (44_100, 96_000), Some((32, 4_096)), SampleFormat::F32),
        range(2, (8_000, 8_000), Some((32, 4_096)), SampleFormat::F32),
    ];
    assert_eq!(
        choose_config("Interface", &ranges, 48_000, 256, 2),
        Ok(StreamChoice {
            channels: 2,
            format: SampleFormat::F32
        })
    );
    assert_eq!(
        choose_config("Interface", &ranges[..2], 48_000, 256, 2),
        Ok(StreamChoice {
            channels: 8,
            format: SampleFormat::F32
        }),
        "a float format beats fewer channels"
    );
    assert_eq!(
        choose_config("Interface", &ranges[1..2], 48_000, 1_024, 2),
        Ok(StreamChoice {
            channels: 2,
            format: SampleFormat::I16
        })
    );
    assert_eq!(
        choose_config(
            "Interface",
            &[range(2, (48_000, 48_000), None, SampleFormat::I32)],
            48_000,
            64,
            2
        ),
        Ok(StreamChoice {
            channels: 2,
            format: SampleFormat::I32
        }),
        "an unknown buffer range is tried as is"
    );
}

#[test]
fn a_device_that_cannot_run_at_the_songs_rate_says_which_rates_it_can() {
    let ranges = [
        range(2, (44_100, 44_100), None, SampleFormat::F32),
        range(2, (48_000, 96_000), None, SampleFormat::F32),
        range(2, (48_000, 96_000), None, SampleFormat::I16),
    ];
    let error = choose_config("Speakers", &ranges, 8_000, 256, 2).unwrap_err();
    assert_eq!(
        error,
        OutputError::UnsupportedSampleRate {
            device: "Speakers".to_owned(),
            requested: 8_000,
            supported: vec![(44_100, 44_100), (48_000, 96_000)],
        }
    );
    assert_eq!(
        error.to_string(),
        "audio device \"Speakers\" cannot play at 8000 Hz, the song's sample rate (nothing resamples); it supports 44100 Hz, 48000 to 96000 Hz"
    );
}

#[test]
fn a_mono_device_a_buffer_out_of_range_or_an_unknown_format_is_refused() {
    assert_eq!(
        choose_config(
            "Mono",
            &[range(1, (48_000, 48_000), None, SampleFormat::F32)],
            48_000,
            256,
            2
        ),
        Err(OutputError::TooFewChannels {
            device: "Mono".to_owned(),
            channels: 1,
            needed: 2
        })
    );
    let error = choose_config(
        "Small",
        &[range(
            2,
            (48_000, 48_000),
            Some((128, 512)),
            SampleFormat::F32,
        )],
        48_000,
        1_024,
        2,
    )
    .unwrap_err();
    assert_eq!(
        error,
        OutputError::UnsupportedBufferSize {
            device: "Small".to_owned(),
            requested: 1_024,
            min: 128,
            max: 512
        }
    );
    assert_eq!(
        error.to_string(),
        "audio device \"Small\" cannot use a buffer of 1024 frames; it supports 128 to 512"
    );
    assert_eq!(
        choose_config(
            "Odd",
            &[range(2, (48_000, 48_000), None, SampleFormat::U64)],
            48_000,
            256,
            2
        ),
        Err(OutputError::UnsupportedSampleFormat {
            device: "Odd".to_owned()
        })
    );
}

#[test]
fn stereo_goes_to_outputs_one_and_two_and_the_other_channels_are_silent() {
    // 5 frames of a 4-channel device, rendered in stereo pieces of 2 frames.
    let mut data = [9.0f32; 20];
    let mut stereo = [0.0f32; 4];
    let mut next = 0.0f32;
    let mut pieces = Vec::new();
    write_interleaved(&mut data, 4, &mut stereo, 2, |piece: &mut [f32]| {
        pieces.push(piece.len() / 2);
        for sample in piece.iter_mut() {
            next += 1.0;
            *sample = next;
        }
    });
    assert_eq!(pieces, vec![2, 2, 1], "no piece is larger than the scratch");
    assert_eq!(
        data,
        [
            1.0, 2.0, 0.0, 0.0, 3.0, 4.0, 0.0, 0.0, 5.0, 6.0, 0.0, 0.0, 7.0, 8.0, 0.0, 0.0, 9.0,
            10.0, 0.0, 0.0
        ]
    );
}

#[test]
fn integer_devices_get_the_converted_samples() {
    let mut data = [7i16; 6];
    let mut stereo = [0.0f32; 8];
    write_interleaved(&mut data, 3, &mut stereo, 2, |piece: &mut [f32]| {
        piece.copy_from_slice(&[0.5, -1.0, 0.0, 1.0]);
    });
    assert_eq!(data, [16_384, -32_768, 0, 0, 32_767, 0]);
}

#[test]
fn a_request_for_n_channels_opens_the_fewest_the_device_has_at_or_above_n() {
    let ranges = [
        range(8, (44_100, 96_000), None, SampleFormat::F32),
        range(2, (44_100, 96_000), None, SampleFormat::F32),
        range(1, (44_100, 96_000), None, SampleFormat::F32),
        range(4, (44_100, 96_000), None, SampleFormat::F32),
        range(16, (44_100, 96_000), None, SampleFormat::I16),
    ];
    let opened = |channels| {
        choose_config("Interface", &ranges, 48_000, 256, channels).map(|choice| choice.channels)
    };
    assert_eq!(opened(1), Ok(1), "a mono request opens the mono range");
    assert_eq!(opened(2), Ok(2));
    assert_eq!(opened(3), Ok(4));
    assert_eq!(opened(5), Ok(8));
    assert_eq!(opened(8), Ok(8));
    assert_eq!(opened(9), Ok(16), "only an integer range has 9 or more");
    let error = choose_config("Interface", &ranges, 48_000, 256, 17).unwrap_err();
    assert_eq!(
        error,
        OutputError::TooFewChannels {
            device: "Interface".to_owned(),
            channels: 16,
            needed: 17
        }
    );
    assert_eq!(
        error.to_string(),
        "audio device \"Interface\" has 16 output channel(s); the routing needs 17"
    );
}

#[test]
fn rendered_channels_go_to_the_first_outputs_and_the_others_are_silent() {
    // 3 frames of a 4-channel device, rendered in 3 channels, 2 frames at
    // a time.
    let mut data = [9.0f32; 12];
    let mut rendered = [0.0f32; 6];
    let mut next = 0.0f32;
    let mut pieces = Vec::new();
    write_interleaved(&mut data, 4, &mut rendered, 3, |piece: &mut [f32]| {
        pieces.push(piece.len() / 3);
        for sample in piece.iter_mut() {
            next += 1.0;
            *sample = next;
        }
    });
    assert_eq!(pieces, vec![2, 1]);
    assert_eq!(
        data,
        [1.0, 2.0, 3.0, 0.0, 4.0, 5.0, 6.0, 0.0, 7.0, 8.0, 9.0, 0.0]
    );

    // A mono device takes the one rendered channel.
    let mut data = [9i16; 2];
    let mut rendered = [0.0f32; 4];
    write_interleaved(&mut data, 1, &mut rendered, 1, |piece: &mut [f32]| {
        piece.copy_from_slice(&[0.5, -1.0][..piece.len()]);
    });
    assert_eq!(data, [16_384, -32_768]);
}

#[test]
fn a_lost_device_or_an_invalidated_stream_stops_the_session_but_an_xrun_does_not() {
    for (kind, reason) in [
        (ErrorKind::DeviceNotAvailable, LossReason::Unplugged),
        (ErrorKind::StreamInvalidated, LossReason::StreamInvalidated),
        (ErrorKind::DeviceChanged, LossReason::DeviceChanged),
        (ErrorKind::DeviceBusy, LossReason::DeviceBusy),
        (ErrorKind::HostUnavailable, LossReason::HostStopped),
        (ErrorKind::BackendError, LossReason::Failed),
    ] {
        assert_eq!(loss_reason(kind), Some(reason), "{kind:?}");
    }
    assert_eq!(
        LossReason::StreamInvalidated.to_string(),
        "the system invalidated the stream, for example because the device's sample rate changed"
    );
    assert_eq!(loss_reason(ErrorKind::Xrun), None);
    assert_eq!(loss_reason(ErrorKind::RealtimeDenied), None);
}
