//! S3: a song's waveform overview is the peak level of its `track` stems
//! mixed at unity, 100 values a second, cached in the song's `waveforms/`
//! folder while its stem files are unchanged. The cache is safe to delete:
//! a missing, stale or damaged one is rebuilt, and one that cannot be
//! written still gives the overview.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use common::{flac_bytes, manifest, write_flac, write_float, write_int, write_manifest};
use domain::StemRole;
use library::{
    LoadError, OVERVIEW_FILE, OVERVIEW_VALUES_PER_SECOND, Overview, OverviewError,
    OverviewProgress, WAVEFORMS_FOLDER, build_overview, load_overview,
};

/// At 1 kHz a value covers 10 frames.
const RATE: u32 = 1_000;
const WINDOW: usize = 10;

fn build(dir: &Path) -> Overview {
    build_overview(dir, &mut |_| {}, &AtomicBool::new(false)).unwrap()
}

fn assert_levels(overview: &Overview, expected: &[f32]) {
    let levels: Vec<f32> = overview.levels().collect();
    assert_eq!(levels.len(), expected.len(), "{levels:?}");
    assert_eq!(overview.len(), expected.len());
    for (level, expected_level) in levels.iter().zip(expected) {
        assert!(
            (level - expected_level).abs() < 1e-4,
            "{levels:?} is not {expected:?}"
        );
    }
}

/// Mono samples, `WINDOW` frames per entry of `peaks`: the third frame of a
/// window is its peak and the others are half of it, so a window's level is
/// its peak only when the sum is taken frame by frame.
fn mono(peaks: &[f32]) -> Vec<f32> {
    peaks
        .iter()
        .flat_map(|&peak| (0..WINDOW).map(move |frame| if frame == 2 { peak } else { peak / 2.0 }))
        .collect()
}

/// Interleaved stereo samples from the mono samples of each side.
fn stereo(left: &[f32], right: &[f32]) -> Vec<f32> {
    left.iter().zip(right).flat_map(|(&l, &r)| [l, r]).collect()
}

/// A song in `dir` with a stereo and a mono track stem, a click and a
/// guide, five windows long; returns the overview's expected levels.
fn mixed_song(dir: &Path) -> Vec<f32> {
    let keys = stereo(
        &mono(&[0.25, 0.0, 0.0, 0.1, 0.25]),
        &mono(&[0.0, -0.375, 0.0, 0.0, 0.0]),
    );
    write_float(&dir.join("stems/keys.wav"), 2, RATE, &keys);
    write_float(
        &dir.join("stems/bass.wav"),
        1,
        RATE,
        &mono(&[0.25, 0.0, -0.125, 0.0, -0.25]),
    );
    write_float(&dir.join("stems/click.wav"), 1, RATE, &[0.9; 50]);
    write_float(&dir.join("stems/guide.wav"), 1, RATE, &[0.8; 50]);
    write_manifest(
        dir,
        &manifest(
            RATE,
            &[
                ("keys", "stems/keys.wav", StemRole::Track),
                ("bass", "stems/bass.wav", StemRole::Track),
                ("click", "stems/click.wav", StemRole::Click),
                ("guide", "stems/guide.wav", StemRole::Guide),
            ],
        ),
    );
    // Left: 0.5, 0, -0.125, 0.1, 0; right: 0.25, -0.375, -0.125, 0, -0.25.
    vec![0.5, 0.375, 0.125, 0.1, 0.25]
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

fn set_modified(path: &Path, time: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// File names in `dir`, sorted; none when it does not exist.
fn entries(dir: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn the_overview_is_the_peak_of_the_track_stems_mixed_at_unity_without_click_or_guide() {
    let dir = tempfile::tempdir().unwrap();
    let expected = mixed_song(dir.path());

    let overview = build(dir.path());

    assert_eq!(overview.sample_rate(), RATE);
    assert_eq!(
        overview.frames_per_value(),
        RATE / OVERVIEW_VALUES_PER_SECOND
    );
    assert_eq!(overview.length_frames(), 50);
    assert_eq!(overview.peak(), 0.5);
    assert_levels(&overview, &expected);
}

#[test]
fn the_level_of_a_frame_range_is_the_loudest_value_it_covers() {
    let dir = tempfile::tempdir().unwrap();
    mixed_song(dir.path());
    let overview = build(dir.path());

    let level = |range| overview.range_level(range);
    let close = |range: std::ops::Range<u64>, expected: f32| {
        let found = level(range.clone());
        assert!((found - expected).abs() < 1e-4, "{range:?}: {found}");
    };
    assert_eq!(level(0..10), 0.5, "the loudest value is the peak");
    close(10..20, 0.375);
    close(15..25, 0.375);
    close(19..21, 0.375);
    close(20..40, 0.125);
    close(30..31, 0.1);
    assert_eq!(level(0..50), 0.5);
    close(45..1_000, 0.25);
    assert_eq!(level(30..30), 0.0, "an empty range");
    assert_eq!(level(50..60), 0.0, "past the song's end");
}

#[test]
fn wav_and_flac_stems_give_the_same_overview() {
    const FLAC_RATE: u32 = 8_000;
    // Three FLAC blocks and part of a fourth, so the stems are read in
    // several blocks.
    const FRAMES: usize = 3 * 4_096 + 1_234;
    let tone = |channels: usize, hz: f64| -> Vec<i32> {
        (0..FRAMES)
            .flat_map(|frame| {
                (0..channels).map(move |channel| {
                    let t = frame as f64 / f64::from(FLAC_RATE);
                    let hz = hz * (channel + 1) as f64;
                    ((t * hz * std::f64::consts::TAU).sin() * 12_000.0) as i32
                })
            })
            .collect()
    };
    let (keys, bass) = (tone(2, 220.0), tone(1, 55.0));
    let stems = |extension: &str| {
        manifest(
            FLAC_RATE,
            &[
                ("keys", &format!("keys.{extension}"), StemRole::Track),
                ("bass", &format!("bass.{extension}"), StemRole::Track),
            ],
        )
    };
    let wav = tempfile::tempdir().unwrap();
    write_int(&wav.path().join("keys.wav"), 16, 2, FLAC_RATE, &keys);
    write_int(&wav.path().join("bass.wav"), 16, 1, FLAC_RATE, &bass);
    write_manifest(wav.path(), &stems("wav"));
    let flac = tempfile::tempdir().unwrap();
    write_flac(&flac.path().join("keys.flac"), 16, 2, FLAC_RATE, &keys);
    write_flac(&flac.path().join("bass.flac"), 16, 1, FLAC_RATE, &bass);
    write_manifest(flac.path(), &stems("flac"));

    let mut updates = Vec::new();
    let from_wav = build_overview(
        wav.path(),
        &mut |progress| updates.push(progress),
        &AtomicBool::new(false),
    )
    .unwrap();
    let from_flac = build(flac.path());

    assert_eq!(from_wav, from_flac);
    assert_eq!(from_wav.len(), FRAMES.div_ceil(80));
    assert!(from_wav.peak() > 0.5, "{}", from_wav.peak());
    assert!(updates.len() > 1, "{updates:?}");
    assert!(updates.windows(2).all(|pair| pair[0].done < pair[1].done));
    assert_eq!(
        updates.last(),
        Some(&OverviewProgress {
            done: FRAMES as u64,
            total: FRAMES as u64
        })
    );
}

#[test]
fn stems_of_different_lengths_mix_to_the_longest() {
    let dir = tempfile::tempdir().unwrap();
    write_float(&dir.path().join("short.wav"), 1, RATE, &[0.5; 20]);
    write_float(&dir.path().join("long.wav"), 1, RATE, &[0.25; 55]);
    write_manifest(
        dir.path(),
        &manifest(
            RATE,
            &[
                ("short", "short.wav", StemRole::Track),
                ("long", "long.wav", StemRole::Track),
            ],
        ),
    );

    let overview = build(dir.path());

    assert_eq!(overview.length_frames(), 55);
    // The last value covers the last 5 frames.
    assert_levels(&overview, &[0.75, 0.75, 0.25, 0.25, 0.25, 0.25]);
}

#[test]
fn a_song_without_track_stems_gives_a_silent_overview() {
    let dir = tempfile::tempdir().unwrap();
    write_float(&dir.path().join("click.wav"), 1, RATE, &[0.9; 30]);
    write_float(&dir.path().join("guide.wav"), 1, RATE, &[0.8; 30]);
    write_manifest(
        dir.path(),
        &manifest(
            RATE,
            &[
                ("click", "click.wav", StemRole::Click),
                ("guide", "guide.wav", StemRole::Guide),
            ],
        ),
    );

    let overview = build(dir.path());

    assert_eq!(overview.length_frames(), 30);
    assert_eq!(overview.peak(), 0.0);
    assert_levels(&overview, &[0.0, 0.0, 0.0]);
    assert_eq!(overview.range_level(0..30), 0.0);
}

#[test]
fn a_fresh_cache_is_read_without_decoding_the_stems() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    let expected = mixed_song(song);
    assert_eq!(load_overview(song), None, "no cache yet");

    let built = build(song);
    assert!(song.join(OVERVIEW_FILE).is_file());
    assert_eq!(entries(&song.join(WAVEFORMS_FOLDER)), ["overview.bin"]);

    // Other audio of the same size, with the modified time the cache saw.
    let bass = song.join("stems/bass.wav");
    let seen = modified(&bass);
    write_float(&bass, 1, RATE, &mono(&[0.0; 5]));
    set_modified(&bass, seen);

    assert_eq!(load_overview(song), Some(built.clone()));
    assert_levels(&built, &expected);
    assert_ne!(build(song), built, "the bass did change");
}

#[test]
fn a_stem_whose_size_modified_time_name_or_role_changed_makes_the_cache_stale() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    mixed_song(song);
    let bass = song.join("stems/bass.wav");

    // The modified time alone.
    build(song);
    set_modified(&bass, modified(&bass) + Duration::from_secs(10));
    assert_eq!(load_overview(song), None, "modified time");
    let rebuilt = build(song);
    assert_eq!(load_overview(song), Some(rebuilt));

    // The size alone.
    let seen = modified(&bass);
    write_float(&bass, 1, RATE, &[0.25; 60]);
    set_modified(&bass, seen);
    assert_eq!(load_overview(song), None, "size");
    let longer = build(song);
    assert_eq!(longer.length_frames(), 60);
    assert_eq!(load_overview(song), Some(longer));

    // The same file under another name.
    let mut package = manifest(
        RATE,
        &[
            ("keys", "stems/keys.wav", StemRole::Track),
            ("bass", "stems/bass copy.wav", StemRole::Track),
            ("click", "stems/click.wav", StemRole::Click),
            ("guide", "stems/guide.wav", StemRole::Guide),
        ],
    );
    fs::copy(&bass, song.join("stems/bass copy.wav")).unwrap();
    set_modified(&song.join("stems/bass copy.wav"), modified(&bass));
    write_manifest(song, &package);
    assert_eq!(load_overview(song), None, "name");
    build(song);
    assert!(load_overview(song).is_some());

    // The keys stem is now the guide, so it is no longer mixed.
    package.stems.retain(|stem| stem.id != "guide");
    package.stems[0].role = StemRole::Guide;
    write_manifest(song, &package);
    assert_eq!(load_overview(song), None, "role");
    let bass_only = build(song);
    assert_eq!(bass_only.peak(), 0.25);
    assert_eq!(load_overview(song), Some(bass_only));
}

#[test]
fn a_damaged_truncated_or_unknown_cache_is_stale_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    mixed_song(song);
    let built = build(song);
    let cache = song.join(OVERVIEW_FILE);
    let good = fs::read(&cache).unwrap();
    let with = |at: usize, bytes: &[u8]| {
        let mut changed = good.clone();
        changed[at..at + bytes.len()].copy_from_slice(bytes);
        changed
    };
    let flipped = |at: usize| with(at, &[good[at] ^ 0x40]);
    let damaged: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("half", good[..good.len() / 2].to_vec()),
        ("one byte short", good[..good.len() - 1].to_vec()),
        ("one byte more", [good.as_slice(), &[0]].concat()),
        ("another magic", flipped(0)),
        ("the next version", with(4, &2u32.to_le_bytes())),
        ("a header byte", flipped(9)),
        ("a value", flipped(good.len() - 12)),
        ("the checksum", flipped(good.len() - 1)),
        ("garbage", vec![0xFF; good.len()]),
        (
            "a valid start then garbage",
            [&good[..8], &[0xFF; 256][..]].concat(),
        ),
    ];
    for (what, bytes) in damaged {
        fs::write(&cache, &bytes).unwrap();
        assert_eq!(load_overview(song), None, "{what}");
        assert_eq!(build(song), built, "{what}");
        assert_eq!(fs::read(&cache).unwrap(), good, "{what}");
        assert_eq!(load_overview(song), Some(built.clone()), "{what}");
    }

    // A deleted cache, or a whole deleted folder, is rebuilt.
    fs::remove_dir_all(song.join(WAVEFORMS_FOLDER)).unwrap();
    assert_eq!(load_overview(song), None);
    assert_eq!(build(song), built);
    assert_eq!(fs::read(&cache).unwrap(), good);

    // A folder where the cache goes is never read; the overview still comes.
    fs::remove_file(&cache).unwrap();
    fs::create_dir(&cache).unwrap();
    assert_eq!(load_overview(song), None);
    assert_eq!(build(song), built);
}

#[cfg(unix)]
#[test]
fn a_cache_link_out_of_the_song_folder_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("Song.song");
    fs::create_dir(&song).unwrap();
    mixed_song(&song);
    let built = build(&song);
    let outside = dir.path().join("outside.bin");
    fs::rename(song.join(OVERVIEW_FILE), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, song.join(OVERVIEW_FILE)).unwrap();

    assert_eq!(load_overview(&song), None);
    assert_eq!(build(&song), built);
    // The build replaced the link and left the file it pointed at alone.
    assert!(
        !fs::symlink_metadata(song.join(OVERVIEW_FILE))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(&outside).unwrap(),
        fs::read(song.join(OVERVIEW_FILE)).unwrap()
    );
    assert_eq!(load_overview(&song), Some(built));
}

#[cfg(unix)]
#[test]
fn a_read_only_library_still_gives_the_overview_without_a_cache() {
    use std::os::unix::fs::PermissionsExt;

    let mode = |path: &Path, mode| fs::set_permissions(path, fs::Permissions::from_mode(mode));
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    let expected = mixed_song(song);
    let waveforms = song.join(WAVEFORMS_FOLDER);
    fs::create_dir(&waveforms).unwrap();
    mode(&waveforms, 0o555).unwrap();
    if fs::write(waveforms.join("probe"), b"").is_ok() {
        // Running as root: the folder stays writable.
        mode(&waveforms, 0o755).unwrap();
        return;
    }

    let result = build_overview(song, &mut |_| {}, &AtomicBool::new(false));
    mode(&waveforms, 0o755).unwrap();

    assert_levels(&result.unwrap(), &expected);
    assert_eq!(entries(&waveforms), Vec::<String>::new());
    assert_eq!(load_overview(song), None);

    // A read-only song folder, where the waveforms folder cannot be made.
    fs::remove_dir(&waveforms).unwrap();
    mode(song, 0o555).unwrap();
    let result = build_overview(song, &mut |_| {}, &AtomicBool::new(false));
    mode(song, 0o755).unwrap();
    assert_levels(&result.unwrap(), &expected);
    assert!(!waveforms.exists());
}

/// A song of 2.5 blocks of 4 096 frames.
fn long_song(dir: &Path) {
    write_int(&dir.join("track.wav"), 16, 1, RATE, &[1_000; 10_240]);
    write_manifest(
        dir,
        &manifest(RATE, &[("track", "track.wav", StemRole::Track)]),
    );
}

#[test]
fn a_cancelled_build_leaves_no_cache_file() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    long_song(song);

    let cancel = AtomicBool::new(true);
    let before = build_overview(song, &mut |_| {}, &cancel);
    assert!(
        matches!(before, Err(OverviewError::Cancelled)),
        "{before:?}"
    );
    assert!(
        !song.join(WAVEFORMS_FOLDER).exists(),
        "cancelled before a block"
    );

    let cancel = AtomicBool::new(false);
    let mut blocks = 0;
    let during = build_overview(
        song,
        &mut |_| {
            blocks += 1;
            cancel.store(true, Ordering::Relaxed);
        },
        &cancel,
    );
    assert!(
        matches!(during, Err(OverviewError::Cancelled)),
        "{during:?}"
    );
    assert_eq!(blocks, 1, "the cancel is seen before the next block");
    assert!(
        !song.join(WAVEFORMS_FOLDER).exists(),
        "cancelled after a block"
    );

    let cancel = AtomicBool::new(false);
    let last = build_overview(
        song,
        &mut |progress| {
            if progress.done == progress.total {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    );
    assert!(matches!(last, Err(OverviewError::Cancelled)), "{last:?}");
    assert!(
        !song.join(WAVEFORMS_FOLDER).exists(),
        "cancelled after the last block"
    );

    // A stale cache stays as it was.
    build(song);
    let stale = fs::read(song.join(OVERVIEW_FILE)).unwrap();
    set_modified(
        &song.join("track.wav"),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1),
    );
    let cancel = AtomicBool::new(true);
    assert!(build_overview(song, &mut |_| {}, &cancel).is_err());
    assert_eq!(fs::read(song.join(OVERVIEW_FILE)).unwrap(), stale);
    assert_eq!(entries(&song.join(WAVEFORMS_FOLDER)), ["overview.bin"]);
}

#[test]
fn a_failed_build_leaves_no_cache_file() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    long_song(song);
    // The header still declares every frame the file no longer holds.
    let track = song.join("track.wav");
    let bytes = fs::read(&track).unwrap();
    fs::write(&track, &bytes[..bytes.len() - 1_000]).unwrap();

    let result = build_overview(song, &mut |_| {}, &AtomicBool::new(false));

    assert!(
        matches!(&result, Err(OverviewError::Load(LoadError::Decode { id, .. })) if id == "track"),
        "{result:?}"
    );
    assert!(!song.join(WAVEFORMS_FOLDER).exists());

    // A folder that is not a song.
    let empty = tempfile::tempdir().unwrap();
    let result = build_overview(empty.path(), &mut |_| {}, &AtomicBool::new(false));
    assert!(
        matches!(
            &result,
            Err(OverviewError::Load(LoadError::ReadManifest { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(load_overview(empty.path()), None);
    assert!(!empty.path().join(WAVEFORMS_FOLDER).exists());
}

#[test]
fn the_cache_of_a_five_minute_song_of_24_stems_stays_under_100_kb() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path();
    let frames = 5 * 60 * RATE as usize;
    let first = song.join("stems/track-01.wav");
    write_int(&first, 16, 1, RATE, &vec![100; frames]);
    let mut stems = Vec::new();
    for n in 1..=24 {
        let file = format!("stems/track-{n:02}.wav");
        if n > 1 {
            fs::copy(&first, song.join(&file)).unwrap();
        }
        stems.push((format!("track-{n:02}"), file));
    }
    let stems: Vec<(&str, &str, StemRole)> = stems
        .iter()
        .map(|(id, file)| (id.as_str(), file.as_str(), StemRole::Track))
        .collect();
    write_manifest(song, &manifest(RATE, &stems));

    let overview = build(song);

    assert_eq!(overview.len(), 5 * 60 * OVERVIEW_VALUES_PER_SECOND as usize);
    let size = fs::metadata(song.join(OVERVIEW_FILE)).unwrap().len();
    assert!(size < 100_000, "{size} bytes");
}

/// The build time of 24 stereo 48 kHz stems of 5 minutes, as 16-bit WAV
/// and as 16-bit FLAC. Run it with
/// `cargo test --release -p library --test waveform -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement: writes about 60 MB of stems and reads 24 of them"]
fn measure_building_the_overview_of_24_stereo_stems_of_5_minutes() {
    const BENCH_RATE: u32 = 48_000;
    let frames = 5 * 60 * BENCH_RATE as usize;
    let samples: Vec<i32> = (0..frames)
        .flat_map(|frame| {
            let t = frame as f64 / f64::from(BENCH_RATE);
            let left = ((t * 220.0 * std::f64::consts::TAU).sin() * 8_000.0) as i32;
            let right = ((t * 330.0 * std::f64::consts::TAU).sin() * 8_000.0) as i32;
            [left, right]
        })
        .collect();
    for extension in ["wav", "flac"] {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path();
        let first = song.join(format!("stems/track-01.{extension}"));
        if extension == "wav" {
            write_int(&first, 16, 2, BENCH_RATE, &samples);
        } else {
            fs::create_dir_all(first.parent().unwrap()).unwrap();
            fs::write(&first, flac_bytes(16, 2, BENCH_RATE, 4_096, &samples)).unwrap();
        }
        let mut stems = Vec::new();
        for n in 1..=24 {
            let file = format!("stems/track-{n:02}.{extension}");
            if n > 1 {
                fs::copy(&first, song.join(&file)).unwrap();
            }
            stems.push((format!("track-{n:02}"), file));
        }
        let stems: Vec<(&str, &str, StemRole)> = stems
            .iter()
            .map(|(id, file)| (id.as_str(), file.as_str(), StemRole::Track))
            .collect();
        write_manifest(song, &manifest(BENCH_RATE, &stems));

        let started = Instant::now();
        let overview = build(song);
        let built = started.elapsed();
        let started = Instant::now();
        let loaded = load_overview(song);
        let read = started.elapsed();

        assert_eq!(loaded.as_ref(), Some(&overview));
        let size = fs::metadata(song.join(OVERVIEW_FILE)).unwrap().len();
        eprintln!(
            "{extension}: built in {built:.2?}, cache of {size} bytes read in {read:.2?}, {} values",
            overview.len()
        );
    }
}
