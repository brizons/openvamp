//! A WAV header that claims far more samples than its file holds must not make
//! the loader reserve memory for them. A truncated or corrupt FLAC file, or one
//! whose STREAMINFO holds zero or absurd values, gives an error: never a panic,
//! a hang or a large allocation.
//!
//! This test binary installs a global allocator that records the largest
//! single allocation; `unsafe` is needed only to forward to the system
//! allocator and stays in this test.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::{flac_bytes, manifest, write_int, write_manifest};
use domain::{StemRole, ValidationError};
use library::{LoadError, SongProblem, StreamConfig, load_song, open_streaming};

struct Tracking;

thread_local! {
    /// Largest allocation on this thread while tracking.
    static LARGEST: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record(size: usize) {
    let _ = LARGEST.try_with(|largest| {
        if let Some(current) = largest.get() {
            largest.set(Some(current.max(size)));
        }
    });
}

// SAFETY: every call forwards to `System` with the caller's arguments.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; `ptr` came from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

#[test]
fn a_header_claiming_billions_of_samples_reserves_only_what_the_file_holds() {
    // 192 kHz keeps the claimed 2 147 483 640 frames under the 24-hour cap,
    // so the stem passes validation and reaches the decoder.
    let rate = 192_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_manifest(
        path,
        &manifest(rate, &[("hostile", "hostile.wav", StemRole::Track)]),
    );
    let file = path.join("hostile.wav");
    write_int(&file, 16, 1, rate, &[0; 1_000]);
    let mut bytes = fs::read(&file).unwrap();
    // 16-bit mono uses the 44-byte PCM header: the data size is at 40..44.
    bytes[40..44].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    fs::write(&file, bytes).unwrap();

    LARGEST.with(|largest| largest.set(Some(0)));
    let result = load_song(path);
    let largest = LARGEST.with(|largest| largest.replace(None)).unwrap_or(0);

    assert!(
        matches!(&result, Err(LoadError::Decode { id, .. }) if id == "hostile"),
        "{result:?}"
    );
    assert!(
        largest < 1 << 20,
        "largest allocation was {largest} bytes for a 2 KB file"
    );
}

/// What loading and streaming a song folder gave, and the largest allocation
/// either made on the thread that called it.
struct Outcome {
    loaded: Result<(), LoadError>,
    streamed: Result<(), LoadError>,
    largest: usize,
}

/// Loads and streams the song at `path` on another thread, tracking its
/// allocations there; fails the test after ten seconds, so a hang cannot
/// hang the test run.
fn load_and_stream(path: &Path) -> Outcome {
    let path: PathBuf = path.to_owned();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        LARGEST.with(|largest| largest.set(Some(0)));
        let loaded = load_song(&path).map(drop);
        let streamed = open_streaming(&path, &StreamConfig::default()).map(drop);
        let largest = LARGEST.with(|largest| largest.replace(None)).unwrap_or(0);
        let _ = sender.send(Outcome {
            loaded,
            streamed,
            largest,
        });
    });
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("loading and streaming return without hanging")
}

/// A mono 16-bit FLAC song of `frames` frames at `rate`, in blocks of
/// 1 024, as one stem `bad` that `edit` changes before it is written.
fn flac_song(rate: u32, frames: usize, edit: impl FnOnce(&mut Vec<u8>)) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        &manifest(rate, &[("bad", "bad.flac", StemRole::Track)]),
    );
    let samples: Vec<i32> = (0..frames as i32).map(|i| (i * 7_919) % 30_000).collect();
    let mut bytes = flac_bytes(16, 1, rate, 1_024, &samples);
    edit(&mut bytes);
    fs::write(dir.path().join("bad.flac"), bytes).unwrap();
    dir
}

/// Byte offsets in a FLAC file with STREAMINFO as its only metadata block:
/// the 4-byte marker, the 4-byte block header, then the 34-byte STREAMINFO.
const STREAMINFO: usize = 8;
const FIRST_FRAME: usize = STREAMINFO + 34;

/// Sets the 36-bit total sample count of STREAMINFO.
fn set_total_frames(bytes: &mut [u8], total: u64) {
    let at = STREAMINFO + 13;
    bytes[at] = (bytes[at] & 0xF0) | ((total >> 32) as u8 & 0x0F);
    bytes[at + 1..at + 5].copy_from_slice(&(total as u32).to_be_bytes());
}

/// Sets the 20-bit sample rate of STREAMINFO.
fn set_sample_rate(bytes: &mut [u8], rate: u32) {
    let at = STREAMINFO + 10;
    bytes[at] = (rate >> 12) as u8;
    bytes[at + 1] = (rate >> 4) as u8;
    bytes[at + 2] = (bytes[at + 2] & 0x0F) | (((rate & 0x0F) as u8) << 4);
}

fn unreadable_reason(result: &Result<(), LoadError>) -> String {
    match result {
        Err(LoadError::Invalid(problems)) => match &problems[..] {
            [SongProblem::StemUnreadable { id, reason, .. }] if id == "bad" => reason.clone(),
            other => panic!("expected one unreadable stem, got {other:?}"),
        },
        other => panic!("expected an invalid package, got {other:?}"),
    }
}

fn assert_decode_error(result: &Result<(), LoadError>) {
    assert!(
        matches!(result, Err(LoadError::Decode { id, .. }) if id == "bad"),
        "{result:?}"
    );
}

#[test]
fn a_flac_header_claiming_billions_of_frames_reserves_only_what_the_file_holds() {
    // At 96 kHz the claim (about 6 h 13 min) stays under the 24-hour cap,
    // so the stem passes validation and reaches the decoder.
    let dir = flac_song(96_000, 1_000, |bytes| {
        set_total_frames(bytes, 2_147_483_640);
    });

    let outcome = load_and_stream(dir.path());

    assert_decode_error(&outcome.loaded);
    assert_decode_error(&outcome.streamed);
    assert!(outcome.loaded.unwrap_err().to_string().ends_with(
        "cannot be decoded: Ill-formed FLAC file: the file ends before its declared length"
    ));
    assert!(
        outcome.largest < 1 << 20,
        "largest allocation was {} bytes for a 2 KB file",
        outcome.largest
    );
}

#[test]
fn a_truncated_flac_file_is_an_error() {
    let dir = flac_song(8_000, 5_000, |bytes| {
        let half = bytes.len() / 2;
        bytes.truncate(half);
    });
    let outcome = load_and_stream(dir.path());
    assert_decode_error(&outcome.loaded);
    assert_decode_error(&outcome.streamed);
    assert!(outcome.largest < 1 << 20, "{} bytes", outcome.largest);

    // Cut inside STREAMINFO: the file cannot even be opened.
    let dir = flac_song(8_000, 5_000, |bytes| bytes.truncate(STREAMINFO + 20));
    let outcome = load_and_stream(dir.path());
    unreadable_reason(&outcome.loaded);
    unreadable_reason(&outcome.streamed);

    // The marker alone.
    let dir = flac_song(8_000, 5_000, |bytes| bytes.truncate(4));
    let outcome = load_and_stream(dir.path());
    unreadable_reason(&outcome.loaded);
    unreadable_reason(&outcome.streamed);
}

#[test]
fn a_corrupt_flac_frame_is_an_error_never_a_shifted_stem() {
    // Garbage over the second frame: the decoder skips it, so the frames
    // after it would come early; that must be an error instead.
    let dir = flac_song(8_000, 5_000, |bytes| {
        for byte in &mut bytes[FIRST_FRAME + 600..FIRST_FRAME + 900] {
            *byte ^= 0x5A;
        }
    });

    let outcome = load_and_stream(dir.path());

    assert_decode_error(&outcome.loaded);
    assert!(outcome.largest < 1 << 20, "{} bytes", outcome.largest);

    // Streaming opens (the last frame is intact), and the damaged frames
    // play as a decode error, while frames past them still play.
    let mut streams = open_streaming(dir.path(), &StreamConfig::default()).unwrap();
    let source = &mut streams.sources[0];
    let mut out = [0.0; 2 * 64];
    for at in [0, 4_000] {
        source.prefetch(at, &[]);
        assert!(streams.monitor.wait_idle(Duration::from_secs(10)));
        assert_eq!(source.read(at, &mut out), at > 0, "frames at {at}");
    }
    assert!(streams.monitor.decode_errors() > 0);
}

#[test]
fn a_flac_stream_info_with_zero_or_absurd_values_is_refused() {
    let dir = flac_song(8_000, 1_000, |bytes| set_total_frames(bytes, 0));
    let outcome = load_and_stream(dir.path());
    let reason = unreadable_reason(&outcome.loaded);
    assert!(reason.contains("length"), "{reason}");
    unreadable_reason(&outcome.streamed);

    let dir = flac_song(8_000, 1_000, |bytes| set_sample_rate(bytes, 0));
    let outcome = load_and_stream(dir.path());
    unreadable_reason(&outcome.loaded);
    unreadable_reason(&outcome.streamed);

    // A block size below the 16-frame minimum.
    let dir = flac_song(8_000, 1_000, |bytes| {
        bytes[STREAMINFO..STREAMINFO + 4].copy_from_slice(&[0, 0, 0, 0]);
    });
    let outcome = load_and_stream(dir.path());
    unreadable_reason(&outcome.loaded);
    unreadable_reason(&outcome.streamed);

    // The largest length STREAMINFO can hold, 2^36 - 1 frames, is far over
    // the 24-hour cap: the package checks refuse it before any decoding.
    let dir = flac_song(8_000, 1_000, |bytes| set_total_frames(bytes, (1 << 36) - 1));
    let outcome = load_and_stream(dir.path());
    for result in [&outcome.loaded, &outcome.streamed] {
        assert!(
            matches!(result, Err(LoadError::Invalid(problems))
                if matches!(&problems[..], [SongProblem::Validation(ValidationError::SongTooLong { .. })])),
            "{result:?}"
        );
    }
    assert!(outcome.largest < 1 << 20, "{} bytes", outcome.largest);
}
