//! S6: FLAC stems play like WAV stems. The same audio stored as WAV and as
//! FLAC loads to the same samples, streams the same frames from any frame,
//! and gives the listing and the package checks the same facts; the format
//! is chosen from the file's first bytes. The audio is noise, which FLAC
//! cannot compress, a tone and silence, which it compresses to little.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{flac_bytes, manifest, write_flac, write_int, write_manifest};
use domain::{StemRole, ValidationError, ValidationWarning};
use engine::live::StemSource;
use library::{
    DiskMonitor, Library, LoadError, SongProblem, StreamConfig, load_song, open_streaming,
};

const RATE: u32 = 8_000;
/// Frames of every test stem: three FLAC blocks of 4 096 frames and part of
/// a fourth.
const FRAMES: usize = 3 * 4_096 + 1_234;
/// The (bits, channels) formats compared.
const FORMATS: [(u16, u16); 4] = [(16, 1), (16, 2), (24, 1), (24, 2)];
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Deterministic full-scale noise in `bits` bits, starting with the lowest
/// and the highest sample.
fn noise(seed: u64, bits: u16, count: usize) -> Vec<i32> {
    let mut state = seed;
    let mut samples: Vec<i32> = (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 32) as i32) >> (32 - bits)
        })
        .collect();
    samples[0] = -(1 << (bits - 1));
    samples[1] = (1 << (bits - 1)) - 1;
    samples
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

/// A tone at half scale in `bits` bits, a different pitch per channel.
fn tone(bits: u16, channels: u16) -> Vec<i32> {
    let amplitude = f64::from(1 << (bits - 2));
    (0..FRAMES)
        .flat_map(|frame| {
            (0..channels).map(move |channel| {
                let hz = 220.0 * f64::from(channel + 2);
                let t = frame as f64 / f64::from(RATE);
                ((t * hz * std::f64::consts::TAU).sin() * amplitude) as i32
            })
        })
        .collect()
}

/// A signal's samples for a seed, the bits and the channels.
type Signal = fn(u64, u16, u16) -> Vec<i32>;

/// The signals compared, by name.
const SIGNALS: [(&str, Signal); 3] = [
    ("noise", |seed, bits, channels| {
        noise(seed, bits, FRAMES * usize::from(channels))
    }),
    ("tone", |_, bits, channels| tone(bits, channels)),
    ("silence", |_, _, channels| {
        vec![0; FRAMES * usize::from(channels)]
    }),
];

/// Writes one WAV and one FLAC stem of the same audio for every signal and
/// format, `wav-<signal>-<bits>-<channels>` then `flac-…`, in one package.
fn write_pairs(dir: &Path) -> Vec<(String, String)> {
    let mut stems = Vec::new();
    let mut pairs = Vec::new();
    let formats = SIGNALS
        .iter()
        .flat_map(|signal| FORMATS.iter().map(move |format| (signal, format)));
    for (seed, (&(signal, samples), &(bits, channels))) in formats.enumerate() {
        let samples = samples(seed as u64 + 1, bits, channels);
        let wav = format!("wav-{signal}-{bits}-{channels}");
        let flac = format!("flac-{signal}-{bits}-{channels}");
        write_int(
            &dir.join(format!("stems/{wav}.wav")),
            bits,
            channels,
            RATE,
            &samples,
        );
        write_flac(
            &dir.join(format!("stems/{flac}.flac")),
            usize::from(bits),
            usize::from(channels),
            RATE,
            &samples,
        );
        stems.push((wav.clone(), format!("stems/{wav}.wav")));
        stems.push((flac.clone(), format!("stems/{flac}.flac")));
        pairs.push((wav, flac));
    }
    let stems: Vec<(&str, &str, StemRole)> = stems
        .iter()
        .map(|(id, file)| (id.as_str(), file.as_str(), StemRole::Track))
        .collect();
    write_manifest(dir, &manifest(RATE, &stems));
    pairs
}

#[test]
fn wav_and_flac_stems_of_the_same_audio_load_to_identical_samples() {
    let dir = tempfile::tempdir().unwrap();
    let pairs = write_pairs(dir.path());

    let loaded = load_song(dir.path()).expect("a package mixing WAV and FLAC stems loads");

    assert_eq!(loaded.song.length(), FRAMES as u64);
    for (wav, flac) in pairs {
        let stem = |id: &str| loaded.stems.iter().find(|stem| stem.id == id).unwrap();
        let (wav, flac) = (stem(&wav), stem(&flac));
        assert_eq!(
            (flac.channels, flac.frames),
            (wav.channels, wav.frames),
            "{}",
            flac.id
        );
        assert_eq!(bits(&flac.samples), bits(&wav.samples), "{}", flac.id);
        if wav.id.contains("noise") {
            assert!(wav.samples.contains(&-1.0), "{} reaches full scale", wav.id);
        }
    }
}

/// Reads `frames` frames at `at` after the disk thread has buffered them.
fn read(source: &mut dyn StemSource, monitor: &DiskMonitor, at: u64, frames: usize) -> Vec<f32> {
    source.prefetch(at, &[]);
    assert!(monitor.wait_idle(IDLE_TIMEOUT), "the disk thread keeps up");
    let mut out = vec![f32::NAN; 2 * frames];
    assert!(
        source.read(at, &mut out),
        "{} frames at {at} are in memory",
        source.id()
    );
    out
}

#[test]
fn a_streamed_flac_stem_plays_the_frames_of_the_wav_stem_from_any_frame() {
    let dir = tempfile::tempdir().unwrap();
    let pairs = write_pairs(dir.path());
    // Blocks of 1 000 frames start inside the FLAC blocks of 4 096, so most
    // reads start mid-block in the file.
    let config = StreamConfig {
        block_frames: 1_000,
        ..StreamConfig::default()
    };
    let last = FRAMES as u64 - 1;
    let positions = [
        0,          // the start
        6_003,      // the middle of a FLAC block, after a jump forward
        2_500,      // back to an earlier frame
        last,       // the last frame: the rest of the read is silence
        8_192,      // the first frame of a later FLAC block
        7_000,      // the first frame of a later stream block
        last + 100, // past the end: silence
        1,
    ];

    let mut streams = open_streaming(dir.path(), &config).unwrap();
    let ids: Vec<String> = streams
        .sources
        .iter()
        .map(|source| source.id().to_owned())
        .collect();
    let index = |id: &str| ids.iter().position(|found| found == id).unwrap();
    for (wav, flac) in pairs {
        let (wav, flac) = (index(&wav), index(&flac));
        for &at in &positions {
            let expected = read(streams.sources[wav].as_mut(), &streams.monitor, at, 1_500);
            let streamed = read(streams.sources[flac].as_mut(), &streams.monitor, at, 1_500);
            assert_eq!(
                bits(&streamed),
                bits(&expected),
                "{} at frame {at}",
                streams.sources[flac].id()
            );
        }
    }
    assert_eq!(streams.monitor.decode_errors(), 0);
}

#[test]
fn the_listing_and_the_package_checks_see_a_flac_stems_rate_and_length() {
    let library = tempfile::tempdir().unwrap();
    let song = library.path().join("Flac Song.song");
    write_flac(
        &song.join("stems/drums.flac"),
        24,
        2,
        RATE,
        &noise(1, 24, 2 * FRAMES),
    );
    write_manifest(
        &song,
        &manifest(RATE, &[("drums", "stems/drums.flac", StemRole::Track)]),
    );

    let listing = Library::new(library.path()).list_songs().unwrap();

    assert!(listing.unreadable.is_empty(), "{:?}", listing.unreadable);
    let entry = &listing.songs[0];
    assert_eq!(
        (entry.sample_rate, entry.length_frames, entry.stems),
        (RATE, FRAMES as u64, 1)
    );

    // A stem 1.5 s shorter warns; a stem at another rate is refused.
    let short = FRAMES - 3 * RATE as usize / 2;
    write_flac(
        &song.join("stems/keys.flac"),
        16,
        1,
        RATE,
        &noise(2, 16, short),
    );
    write_flac(
        &song.join("stems/bass.flac"),
        16,
        1,
        2 * RATE,
        &noise(3, 16, FRAMES),
    );
    write_manifest(
        &song,
        &manifest(
            RATE,
            &[
                ("drums", "stems/drums.flac", StemRole::Track),
                ("keys", "stems/keys.flac", StemRole::Track),
            ],
        ),
    );
    assert_eq!(
        load_song(&song).unwrap().warnings,
        [ValidationWarning::StemLengthsDiffer {
            shortest: "keys".into(),
            shortest_length: short as u64,
            longest: "drums".into(),
            longest_length: FRAMES as u64,
        }]
    );
    write_manifest(
        &song,
        &manifest(
            RATE,
            &[
                ("drums", "stems/drums.flac", StemRole::Track),
                ("bass", "stems/bass.flac", StemRole::Track),
            ],
        ),
    );
    let Err(LoadError::Invalid(problems)) = load_song(&song) else {
        panic!("a stem at another rate is refused");
    };
    assert_eq!(
        problems,
        [SongProblem::Validation(
            ValidationError::StemSampleRateMismatch {
                id: "bass".into(),
                found: 2 * RATE,
                expected: RATE,
            }
        )]
    );
}

#[test]
fn the_format_comes_from_the_first_bytes_not_the_extension() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let samples = noise(1, 16, 2 * FRAMES);
    fs::create_dir_all(path.join("stems")).unwrap();
    fs::write(
        path.join("stems/flac-inside.wav"),
        flac_bytes(16, 2, RATE, 4_096, &samples),
    )
    .unwrap();
    write_int(&path.join("stems/wav-inside.flac"), 16, 2, RATE, &samples);
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("flac", "stems/flac-inside.wav", StemRole::Track),
                ("wav", "stems/wav-inside.flac", StemRole::Track),
            ],
        ),
    );

    let loaded = load_song(path).expect("both stems load");

    assert_eq!(
        bits(&loaded.stems[0].samples),
        bits(&loaded.stems[1].samples)
    );
}

#[test]
fn a_flac_stem_with_more_than_two_channels_is_refused_naming_its_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_flac(
        &path.join("surround.flac"),
        16,
        3,
        RATE,
        &noise(1, 16, 3 * 4_096),
    );
    write_manifest(
        path,
        &manifest(RATE, &[("surround", "surround.flac", StemRole::Track)]),
    );

    let Err(LoadError::Invalid(problems)) = load_song(path) else {
        panic!("a 3-channel stem is refused");
    };

    let messages: Vec<String> = problems.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "stem \"surround\" file \"surround.flac\" is 16-bit FLAC, 3 channels; supported FLAC stems are 4- to 24-bit, mono or stereo"
        ]
    );
}

/// `tests/fixtures/tone-32bit-stereo.flac` and `tone-32bit-mono-b256.flac`:
/// 5 000 frames at 48 kHz of a 440 Hz sine at 2·10⁹ (the right channel a
/// radian later), encoded from 32-bit WAV by flac 1.5.0 as `flac` and
/// `flac -b 256`. symphonia 0.6.1 needs 33 bits for their side channel and
/// residuals: it decodes them wrong, or panics in a debug build.
#[test]
fn a_flac_stem_deeper_than_24_bits_is_refused_naming_its_format() {
    for (fixture, channels) in [
        ("tone-32bit-stereo.flac", "2 channels"),
        ("tone-32bit-mono-b256.flac", "1 channel"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture),
            path.join("deep.flac"),
        )
        .unwrap();
        write_manifest(
            path,
            &manifest(48_000, &[("deep", "deep.flac", StemRole::Track)]),
        );
        let expected = format!(
            "stem \"deep\" file \"deep.flac\" is 32-bit FLAC, {channels}; supported FLAC stems are 4- to 24-bit, mono or stereo"
        );

        for result in [
            load_song(path).map(drop),
            open_streaming(path, &StreamConfig::default()).map(drop),
        ] {
            let Err(LoadError::Invalid(problems)) = result else {
                panic!("{fixture} is refused: {result:?}");
            };
            let messages: Vec<String> = problems.iter().map(ToString::to_string).collect();
            assert_eq!(messages, [expected.as_str()], "{fixture}");
        }
    }
}
