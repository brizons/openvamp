//! S6: a streamed FLAC stem plays the frames of its WAV twin, whatever its
//! audio compresses to and whatever its SEEKTABLE says.
//!
//! symphonia 0.6.1 keeps the last frame header its parser saw across a seek
//! that ends where its reader already was. Opening a stem for streaming reads
//! its last frame, so the first seek back to the start found that stale
//! header and merged the frames after the first into one packet: frames
//! 4 096 to 6 143 of a short or mostly silent stem played as silence. Its
//! seek also subtracts SEEKTABLE offsets without checking them, which a cut
//! or forged file turns into a panic on the disk thread.

mod common;

use std::f64::consts::TAU;
use std::path::Path;
use std::time::Duration;

use common::{flac_bytes, flac_bytes_with_seek_points, manifest, write_int, write_manifest};
use domain::StemRole;
use library::{StreamConfig, StreamingSong, load_song, open_streaming};

const RATE: u32 = 48_000;
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// A stereo tone, 440 Hz left and 660 Hz right, at `amplitude`.
fn tone(frames: usize, amplitude: f64) -> Vec<i32> {
    (0..frames)
        .flat_map(|frame| {
            let t = frame as f64 / f64::from(RATE);
            [440.0, 660.0].map(|hz| ((t * hz * TAU).sin() * amplitude) as i32)
        })
        .collect()
}

/// A song with the WAV stem `wav` and the FLAC stem `flac` of the same
/// stereo `samples` at `bits`.
fn write_twins(dir: &Path, bits: u16, samples: &[i32], flac: &[u8]) {
    write_int(&dir.join("stems/wav.wav"), bits, 2, RATE, samples);
    std::fs::write(dir.join("stems/flac.flac"), flac).unwrap();
    write_manifest(
        dir,
        &manifest(
            RATE,
            &[
                ("wav", "stems/wav.wav", StemRole::Track),
                ("flac", "stems/flac.flac", StemRole::Track),
            ],
        ),
    );
}

/// Reads 1 024 frames at every frame of `path` from both stems once the
/// disk thread has buffered them; the frames where the FLAC stem's read
/// differs from the WAV stem's, with whether each read was complete.
fn differences(
    streams: &mut StreamingSong,
    path: impl IntoIterator<Item = u64>,
) -> Vec<(u64, bool, bool)> {
    let mut differences = Vec::new();
    for at in path {
        let mut reads = Vec::new();
        for source in &mut streams.sources {
            source.prefetch(at, &[]);
            assert!(streams.monitor.wait_idle(IDLE_TIMEOUT), "the disk keeps up");
            let mut out = vec![f32::NAN; 2 * 1_024];
            let complete = source.read(at, &mut out);
            let bits: Vec<u32> = out.iter().map(|sample| sample.to_bits()).collect();
            reads.push((complete, bits));
        }
        if reads[0] != reads[1] {
            differences.push((at, reads[0].0, reads[1].0));
        }
    }
    differences
}

#[test]
fn a_short_flac_stem_streams_from_the_start_like_its_wav_twin() {
    // 0.42 s of a quiet tone, as a short stinger: about 11 KB of FLAC.
    let frames = 20_000;
    let samples = tone(frames, 3_000.0);
    let dir = tempfile::tempdir().unwrap();
    write_twins(
        dir.path(),
        16,
        &samples,
        &flac_bytes(16, 2, RATE, 4_096, &samples),
    );

    let mut streams = open_streaming(dir.path(), &StreamConfig::default()).unwrap();
    let differences = differences(&mut streams, (0..frames as u64).step_by(1_024));

    assert_eq!(differences, [], "(frame, WAV complete, FLAC complete)");
    assert_eq!(streams.monitor.decode_errors(), 0);
}

#[test]
fn a_flac_stem_with_a_silent_intro_streams_like_its_wav_twin() {
    // 15 s, silent for the first 6 s, with a SEEKTABLE point every 5 s as
    // the reference encoder writes: played from the start, then across the
    // 5 s point into the audible part.
    let frames = 15 * RATE as usize;
    let silent = 6 * RATE as usize;
    let mut samples = tone(frames, 2_000_000.0);
    samples[..2 * silent].fill(0);
    let every = 5 * RATE as u64;
    let flac = flac_bytes_with_seek_points(24, 2, RATE, 4_096, &samples, |blocks| {
        (0..3)
            .filter_map(|n| blocks.iter().rev().find(|(frame, _)| *frame <= n * every))
            .copied()
            .collect()
    });
    let dir = tempfile::tempdir().unwrap();
    write_twins(dir.path(), 24, &samples, &flac);

    let mut streams = open_streaming(dir.path(), &StreamConfig::default()).unwrap();
    let path = (0..20_480)
        .step_by(1_024)
        .chain((every - 10_240..every + 160_000).step_by(1_024));
    let differences = differences(&mut streams, path);

    assert_eq!(differences, [], "(frame, WAV complete, FLAC complete)");
    assert_eq!(streams.monitor.decode_errors(), 0);
}

#[test]
fn a_flac_stem_whose_seek_table_lies_or_is_out_of_order_plays_like_its_wav_twin() {
    let frames = 10 * 4_096 + 1_000;
    let samples = tone(frames, 2_000_000.0);
    let forged: [&[(u64, u64)]; 2] = [
        // Offsets past the end of the file, and frames that do not match.
        &[(0, 0), (8_192, 1 << 40), (16_384, 3), (30_000, 50)],
        // Out of order.
        &[(40_000, 0), (0, 1_000_000)],
    ];
    for points in forged {
        let flac = flac_bytes_with_seek_points(24, 2, RATE, 4_096, &samples, |_| points.to_vec());
        let dir = tempfile::tempdir().unwrap();
        write_twins(dir.path(), 24, &samples, &flac);

        let loaded = load_song(dir.path()).unwrap();
        assert_eq!(
            loaded.stems[1].samples, loaded.stems[0].samples,
            "{points:?}"
        );
        let mut streams = open_streaming(dir.path(), &StreamConfig::default()).unwrap();
        // Forward from the start, then back through every block.
        let path = (0..frames as u64)
            .step_by(1_024)
            .chain((0..frames as u64).rev().step_by(3_001));
        let differences = differences(&mut streams, path);

        assert_eq!(differences, [], "{points:?}");
        assert_eq!(streams.monitor.decode_errors(), 0, "{points:?}");
        assert!(streams.monitor.is_running(), "{points:?}");
    }
}
