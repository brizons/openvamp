//! S1, S3, S4 and S5 (F1.1–F1.5): importing a folder or ZIP of stems as a
//! song package in the library folder.
//!
//! WAV and FLAC sources are generated here. The MP3 and M4A sources are
//! `tests/import/tone.mp3` and `tests/import/tone.m4a`: 96 123 frames of
//! stereo 48 kHz, a 440 Hz sine on the left and a 660 Hz sine on the right
//! at 8 000 / 32 768 ([`tone`] generates the same 16-bit samples), encoded
//! with ffmpeg 9.0.2 as `-c:a libmp3lame -b:a 64k` (with its LAME header) and
//! `-c:a aac -b:a 64k` (with its edit list).
//!
//! Two more record their length in other ways:
//!
//! - `tests/import/apple.m4a`: 60 010 frames of mono 48 kHz silence with one
//!   sample at 0.9 at frame [`IMPULSE`], from ffmpeg's
//!   `aevalsrc='if(eq(n,10007),0.9,0)':s=48000:d=1.2502083` as 16-bit WAV,
//!   encoded with macOS 27.2's `afconvert -f m4af -d aac -b 128000`: Apple's
//!   AAC encoder, which records its delay and padding only in an `iTunSMPB`
//!   tag, without an edit list.
//! - `tests/import/noxing.mp3`: a 440 Hz sine, mono 44.1 kHz, 1.3 s, from
//!   ffmpeg's `sine=f=440:r=44100:d=1.3` encoded as `-c:a libmp3lame -b:a
//!   192k -write_xing 0`: an MP3 without a Xing or LAME header, whose length
//!   is only an estimate until it is decoded.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use common::{flac_bytes, write_flac, write_int};
use domain::{
    Bus, DraftError, DraftWarning, MapChange, Part, SongPackage, StoredFormat, TimeSignature,
    bus_for_stem,
};
use library::{
    ImportDraft, ImportError, ImportProgress, Library, SourceFormat, StemAction, load_song,
    probe_import, song_folder_name,
};

const RATE: u32 = 48_000;

/// Frame of the impulse in `apple.m4a`.
const IMPULSE: usize = 10_007;

/// Frames of the MP3 and M4A fixtures' source.
const TONE_FRAMES: u64 = 96_123;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/import")
        .join(name)
}

/// The 16-bit stereo samples the MP3 and M4A fixtures were encoded from.
fn tone() -> Vec<i32> {
    (0..TONE_FRAMES)
        .flat_map(|frame| {
            let t = frame as f64 / f64::from(RATE);
            [440.0, 660.0]
                .map(|hz| ((t * hz * std::f64::consts::TAU).sin() * 8_000.0).round() as i32)
        })
        .collect()
}

/// A stereo ramp of `frames` frames at `bits` bits, different per channel.
fn ramp(frames: usize, bits: u32) -> Vec<i32> {
    let max = (1i64 << (bits - 1)) - 1;
    (0..frames * 2)
        .map(|i| ((i as i64 * 7_919) % (2 * max) - max) as i32)
        .collect()
}

/// A WAV of `frames` stereo 16-bit frames at `rate`.
fn wav(path: &Path, rate: u32, frames: usize) {
    write_int(path, 16, 2, rate, &ramp(frames, 16));
}

fn no_progress(_: ImportProgress) {}

fn save(library: &Library, draft: &ImportDraft) -> Result<library::SongEntry, ImportError> {
    library.save_import(draft, &mut no_progress, &AtomicBool::new(false))
}

/// Every path under `dir`, relative to it, sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            if path.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn names(draft: &ImportDraft) -> Vec<&str> {
    draft
        .stems()
        .iter()
        .map(|stem| stem.name.as_str())
        .collect()
}

/// Writes a ZIP file with `entries` (name, bytes), deflated.
fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in entries {
        if name.ends_with('/') {
            zip.add_directory(*name, options).unwrap();
        } else {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
    }
    zip.finish().unwrap();
}

fn wav_bytes(rate: u32, frames: usize) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.wav");
    wav(&path, rate, frames);
    fs::read(path).unwrap()
}

/// A WAV file of 64-bit float samples, which the player does not read.
fn float64_wav(channels: u16, rate: u32, samples: &[f64]) -> Vec<u8> {
    let data_len = (samples.len() * 8) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 8).to_le_bytes());
    bytes.extend_from_slice(&(channels * 8).to_le_bytes());
    bytes.extend_from_slice(&64u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// An ID3v2.3 tag with a title, as some taggers put before a FLAC file.
fn id3_tag() -> Vec<u8> {
    let mut frame = b"TIT2".to_vec();
    let text = b"\x00Song title";
    frame.extend_from_slice(&(text.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(text);
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    let size = frame.len() as u32;
    tag.extend_from_slice(&[
        (size >> 21) as u8 & 0x7f,
        (size >> 14) as u8 & 0x7f,
        (size >> 7) as u8 & 0x7f,
        size as u8 & 0x7f,
    ]);
    tag.extend_from_slice(&frame);
    tag
}

/// Decodes `path` with symphonia, gapless, as 24-bit integers scaled to
/// `f32` the way the player converts a 24-bit FLAC: what an import of it
/// must store.
fn decoded_24_bit(path: &Path, extension: &str) -> Vec<f32> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;

    let source =
        MediaSourceStream::new(Box::new(fs::File::open(path).unwrap()), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut reader = symphonia::default::get_probe()
        .probe(&hint, source, FormatOptions::default(), Default::default())
        .unwrap();
    let track = reader.default_track(TrackType::Audio).unwrap();
    let id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(
            track.codec_params.as_ref().unwrap().audio().unwrap(),
            &AudioDecoderOptions::default(),
        )
        .unwrap();
    let mut samples = Vec::new();
    let mut buffer = Vec::<f64>::new();
    while let Ok(Some(packet)) = reader.next_packet() {
        if packet.track_id != id {
            continue;
        }
        decoder
            .decode(&packet)
            .unwrap()
            .copy_to_vec_interleaved(&mut buffer);
        samples.extend(buffer.iter().map(|&s| {
            (s * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as f32 / 8_388_608.0
        }));
    }
    samples
}

/// The frame offset, from -range to range, at which the left channel of
/// `decoded` best matches `expected` (both interleaved stereo). The left
/// channel is a 440 Hz sine, 109.1 frames a cycle, so a range under half a
/// cycle finds one best offset; an untrimmed MP3 or M4A would show the
/// delay left after whole cycles (1 105 frames is 14 more than 10 cycles,
/// 1 024 is 42 more than 9).
fn best_lag(decoded: &[f32], expected: &[i32], range: i64) -> i64 {
    let frames = 4_800i64;
    (-range..=range)
        .min_by(|&a, &b| {
            let error = |lag: i64| -> f64 {
                // Away from the start, which AAC encodes roughly.
                (40_000..40_000 + frames)
                    .map(|frame| {
                        let (d, e) = ((frame + lag) as usize * 2, frame as usize * 2);
                        (f64::from(decoded[d]) - f64::from(expected[e]) / 32_768.0).powi(2)
                    })
                    .sum()
            };
            error(a).total_cmp(&error(b))
        })
        .unwrap()
}

#[test]
fn a_folder_import_takes_every_audio_file_in_its_subfolders_and_skips_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Amazing Grace");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    wav(&source.join("Guitars/EG 1.WAV"), RATE, 1_000);
    write_flac(
        &source.join("Keys/Deep/Pad.flac"),
        24,
        2,
        RATE,
        &ramp(1_000, 24),
    );
    wav(&source.join(".hidden.wav"), RATE, 1_000);
    wav(&source.join(".git/inside.wav"), RATE, 1_000);
    wav(&source.join("__MACOSX/._Drums.wav"), RATE, 1_000);
    fs::write(source.join("notes.txt"), "hello").unwrap();
    fs::write(source.join("Keys/cover.jpg"), [0xff, 0xd8]).unwrap();

    let draft = probe_import(&source).unwrap();

    assert_eq!(
        names(&draft),
        ["Drums.wav", "Guitars/EG 1.WAV", "Keys/Deep/Pad.flac"]
    );
    assert!(draft.refused().is_empty());
    assert_eq!(draft.source(), fs::canonicalize(&source).unwrap());
    assert_eq!(draft.song.title, "Amazing Grace");
    assert_eq!(draft.song.bpm, 120.0);
    assert_eq!(
        draft.song.time_signature,
        TimeSignature {
            numerator: 4,
            denominator: 4
        }
    );
    assert_eq!(draft.song.key, None);
    let formats: Vec<SourceFormat> = draft.stems().iter().map(|stem| stem.format).collect();
    assert_eq!(
        formats,
        [SourceFormat::Wav, SourceFormat::Wav, SourceFormat::Flac]
    );
    for (stem, facts) in draft.stems().iter().zip(draft.song.stems()) {
        assert_eq!(stem.action, StemAction::Copy, "{}", stem.name);
        assert_eq!(stem.channels, 2);
        assert_eq!((facts.sample_rate, facts.length_frames), (RATE, 1_000));
        assert!(facts.automatic);
    }
    let parts: Vec<Part> = draft.song.stems().iter().map(|stem| stem.part).collect();
    assert_eq!(parts, [Part::Drums, Part::ElectricGuitar, Part::Pad]);
}

#[test]
fn a_zip_import_skips_folders_hidden_files_macosx_and_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("Live Set.v2.zip");
    let drums = wav_bytes(RATE, 1_000);
    write_zip(
        &zip,
        &[
            ("Live Set/", b""),
            ("Live Set/Drums.wav", &drums),
            ("Live Set/Guitars/EG 1.wav", &drums),
            ("Live Set/.DS_Store", b"junk"),
            ("__MACOSX/Live Set/._Drums.wav", b"resource fork"),
            ("Live Set/readme.txt", b"hello"),
        ],
    );

    let draft = probe_import(&zip).unwrap();

    assert_eq!(
        names(&draft),
        ["Live Set/Drums.wav", "Live Set/Guitars/EG 1.wav"]
    );
    assert!(draft.refused().is_empty());
    assert_eq!(draft.song.title, "Live Set.v2", "the ZIP name without .zip");
    assert!(
        draft
            .stems()
            .iter()
            .all(|stem| stem.action == StemAction::Copy)
    );
}

#[test]
fn a_source_without_audio_or_that_is_no_folder_or_zip_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("Empty");
    fs::create_dir(&empty).unwrap();
    fs::write(empty.join("notes.txt"), "hello").unwrap();
    let text = dir.path().join("song.txt");
    fs::write(&text, "hello").unwrap();

    assert!(matches!(
        probe_import(&empty),
        Err(ImportError::NoAudio { .. })
    ));
    assert!(matches!(
        probe_import(&text),
        Err(ImportError::NotFolderOrZip { .. })
    ));
    let not_zip = dir.path().join("broken.zip");
    fs::write(&not_zip, "not a zip").unwrap();
    assert!(matches!(
        probe_import(&not_zip),
        Err(ImportError::Zip { .. })
    ));
}

#[test]
fn a_stem_named_eg_1_is_assigned_to_the_electric_guitar_part_and_bus_without_user_action() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("EG 1.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    let stem = &loaded.song.stems()[0];
    assert_eq!(stem.part, "electric-guitar");
    assert_eq!(bus_for_stem(stem), Bus::Guitars);
}

#[test]
fn zip_entry_names_are_never_paths_written() {
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path().join("outer");
    let library_dir = outer.join("Library");
    fs::create_dir_all(&library_dir).unwrap();
    let zip = outer.join("Evil.zip");
    let audio = wav_bytes(RATE, 1_000);
    write_zip(
        &zip,
        &[
            ("../evil.wav", &audio),
            ("/tmp/absolute.wav", &audio),
            ("..\\..\\windows.wav", &audio),
        ],
    );

    let draft = probe_import(&zip).unwrap();
    assert_eq!(
        names(&draft),
        ["../../windows.wav", "../evil.wav", "/tmp/absolute.wav"]
    );
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    assert_eq!(entry.song, "Evil.song");
    assert_eq!(
        tree(&outer),
        [
            "Evil.zip",
            "Library",
            "Library/Evil.song",
            "Library/Evil.song/song.json",
            "Library/Evil.song/stems",
            "Library/Evil.song/stems/absolute.wav",
            "Library/Evil.song/stems/evil.wav",
            "Library/Evil.song/stems/windows.wav",
        ]
    );
    assert!(!dir.path().join("evil.wav").exists());
    assert!(
        !Path::new("/tmp/absolute.wav").exists() || fs::read("/tmp/absolute.wav").unwrap() != audio
    );
}

#[test]
fn a_copied_wav_or_flac_is_byte_identical_and_reopens_with_the_same_samples() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    let int24 = ramp(5_000, 24);
    write_int(&source.join("Bass.wav"), 24, 2, RATE, &int24);
    let flac = flac_bytes(16, 2, RATE, 4_096, &ramp(5_000, 16));
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("Keys.flac"), &flac).unwrap();
    // A FLAC named .wav is copied as the FLAC its content is.
    fs::write(source.join("Pad.wav"), &flac).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    let stored: Vec<StoredFormat> = draft
        .song
        .stems()
        .iter()
        .map(|stem| stem.stored_as)
        .collect();
    assert_eq!(
        stored,
        [StoredFormat::Wav, StoredFormat::Flac, StoredFormat::Flac]
    );
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let song = library_dir.join(&entry.song);
    assert_eq!(
        fs::read(song.join("stems/bass.wav")).unwrap(),
        fs::read(source.join("Bass.wav")).unwrap()
    );
    assert_eq!(fs::read(song.join("stems/keys.flac")).unwrap(), flac);
    assert_eq!(fs::read(song.join("stems/pad.flac")).unwrap(), flac);
    let loaded = load_song(&song).unwrap();
    let expected: Vec<f32> = int24.iter().map(|&s| s as f32 / 8_388_608.0).collect();
    assert_eq!(loaded.stems[0].samples, expected);
    let expected: Vec<f32> = ramp(5_000, 16)
        .iter()
        .map(|&s| s as f32 / 32_768.0)
        .collect();
    assert_eq!(loaded.stems[1].samples, expected);
}

#[test]
fn a_wav_or_flac_the_player_cannot_read_is_transcoded_to_flac_without_losing_a_sample() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    let int8: Vec<i32> = (0..3_000).map(|i| (i * 37) % 256 - 128).collect();
    write_int(&source.join("Shaker.wav"), 8, 1, RATE, &int8);
    let int16 = ramp(6_000, 16);
    let mut tagged = id3_tag();
    tagged.extend(flac_bytes(16, 2, RATE, 4_096, &int16));
    fs::write(source.join("Keys.flac"), tagged).unwrap();
    let floats: Vec<f64> = (0..4_000).map(|i| (i as f64 * 0.01).sin() * 0.5).collect();
    fs::write(source.join("Pad.wav"), float64_wav(2, RATE, &floats)).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();

    assert_eq!(names(&draft), ["Keys.flac", "Pad.wav", "Shaker.wav"]);
    for stem in draft.stems() {
        let StemAction::Transcode { reason } = &stem.action else {
            panic!("{} is transcoded", stem.name);
        };
        assert!(
            reason.starts_with("the player cannot read it as it is"),
            "{reason}"
        );
    }
    let facts: Vec<(StoredFormat, u64, u16)> = draft
        .song
        .stems()
        .iter()
        .zip(draft.stems())
        .map(|(facts, stem)| (facts.stored_as, facts.length_frames, stem.channels))
        .collect();
    assert_eq!(
        facts,
        [
            (StoredFormat::Flac, 6_000, 2),
            (StoredFormat::Flac, 2_000, 2),
            (StoredFormat::Flac, 3_000, 1),
        ]
    );
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    let keys: Vec<f32> = int16.iter().map(|&s| s as f32 / 32_768.0).collect();
    assert_eq!(loaded.stems[0].samples, keys, "16 bits fit in 24");
    let shaker: Vec<f32> = int8.iter().map(|&s| s as f32 / 128.0).collect();
    assert_eq!(loaded.stems[2].samples, shaker, "8 bits fit in 24");
    // 64-bit float is rounded to the nearest 24-bit step.
    for (got, want) in loaded.stems[1].samples.iter().zip(&floats) {
        assert!((f64::from(*got) - want).abs() <= 0.5 / 8_388_608.0 + 1e-12);
    }
}

#[test]
fn an_mp3_or_m4a_keeps_its_source_length_and_start_after_gapless_trimming() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    fs::create_dir(&source).unwrap();
    fs::copy(fixture("tone.mp3"), source.join("Bass.mp3")).unwrap();
    fs::copy(fixture("tone.m4a"), source.join("Keys.m4a")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();

    // Both record their encoder delay and padding (the MP3 in its LAME
    // header, the M4A in its edit list), so the length after trimming is
    // the source's exactly: the formats allow it here.
    let lengths: Vec<u64> = draft
        .song
        .stems()
        .iter()
        .map(|stem| stem.length_frames)
        .collect();
    assert_eq!(lengths, [TONE_FRAMES, TONE_FRAMES]);
    for stem in draft.stems() {
        assert!(
            matches!(stem.action, StemAction::Transcode { .. }),
            "{}",
            stem.name
        );
    }
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    let expected = tone();
    for stem in &loaded.stems {
        assert_eq!(stem.frames, TONE_FRAMES);
        assert_eq!(
            best_lag(&stem.samples, &expected, 50),
            0,
            "{} starts on time",
            stem.id
        );
    }
    // The MP3 stores what its decoder gives, rounded to 24 bits.
    assert_eq!(
        loaded.stems[0].samples,
        decoded_24_bit(&fixture("tone.mp3"), "mp3")
    );
}

#[test]
fn a_sample_rate_mismatch_blocks_the_save_and_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    wav(&source.join("Bass.wav"), RATE, 1_000);
    wav(&source.join("Click.wav"), 44_100, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    let error = save(&Library::new(&library_dir), &draft).unwrap_err();

    let ImportError::Draft(errors) = &error else {
        panic!("{error}");
    };
    assert_eq!(
        errors,
        &[DraftError::SampleRateMismatch {
            file: "Click.wav".to_owned(),
            found: 44_100,
            expected: RATE,
        }]
    );
    assert!(
        error.to_string().contains("\"Click.wav\" is 44100 Hz"),
        "{error}"
    );
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[test]
fn stem_lengths_more_than_a_second_apart_warn_but_save() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 50_000);
    wav(&source.join("Pad.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    let warnings = draft.song.to_package().unwrap().warnings;
    assert_eq!(
        warnings,
        [DraftWarning::StemLengthsDiffer {
            shortest: "Pad.wav".to_owned(),
            shortest_length: 1_000,
            longest: "Drums.wav".to_owned(),
            longest_length: 50_000,
        }]
    );

    let entry = save(&Library::new(&library_dir), &draft).unwrap();
    assert_eq!(entry.length_frames, 50_000);
}

#[test]
fn files_with_more_than_two_channels_or_that_nothing_decodes_are_refused_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    write_int(&source.join("Surround.wav"), 16, 3, RATE, &[0; 300]);
    write_flac(&source.join("Six.flac"), 16, 6, RATE, &[0; 600]);
    fs::write(
        source.join("Noise.wav"),
        b"this is no audio at all, just text",
    )
    .unwrap();
    fs::write(source.join("Fake.mp3"), vec![0x42; 5_000]).unwrap();
    fs::write(source.join("Empty.m4a"), b"").unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();

    assert_eq!(names(&draft), ["Drums.wav"]);
    let refused: Vec<(&str, &str)> = draft
        .refused()
        .iter()
        .map(|file| (file.name.as_str(), file.reason.as_str()))
        .collect();
    assert_eq!(refused.len(), 5, "{refused:?}");
    let reason = |name: &str| refused.iter().find(|(n, _)| *n == name).unwrap().1;
    assert_eq!(
        reason("Surround.wav"),
        "it has 3 channels; stems must be mono or stereo"
    );
    assert_eq!(
        reason("Six.flac"),
        "it has 6 channels; stems must be mono or stereo"
    );
    for name in ["Noise.wav", "Fake.mp3", "Empty.m4a"] {
        assert!(
            reason(name).starts_with("it cannot be decoded: "),
            "{name}: {}",
            reason(name)
        );
    }

    let error = save(&Library::new(&library_dir), &draft).unwrap_err();
    let ImportError::Refused(files) = &error else {
        panic!("{error}");
    };
    assert_eq!(files.len(), 5);
    assert!(
        error
            .to_string()
            .contains("\"Surround.wav\": it has 3 channels"),
        "{error}"
    );
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[test]
fn a_title_becomes_a_safe_folder_name() {
    for (title, name) in [
        ("Amazing Grace", "Amazing Grace"),
        ("AC/DC: Live\\Set", "AC-DC- Live-Set"),
        ("Why? <Not> \"Now\" | * ", "Why Not Now"),
        ("Tab\there\u{7}", "Tabhere"),
        ("  ..Hidden.. ", "Hidden"),
        ("CON", "CON_"),
        ("nul.mix", "nul_.mix"),
        ("Con Dios", "Con Dios"),
        ("", "Untitled"),
        ("...", "Untitled"),
        ("Canción ñandú", "Canción ñandú"),
    ] {
        assert_eq!(song_folder_name(title), name, "{title:?}");
    }
    let long = "é".repeat(150);
    let cut = song_folder_name(&long);
    assert_eq!(cut, "é".repeat(100), "cut to 200 bytes at a character");
}

#[test]
fn a_taken_folder_name_gets_a_number_and_the_new_song_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Hymn");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir_all(library_dir.join("Hymn.song")).unwrap();
    let library = Library::new(&library_dir);

    let draft = probe_import(&source).unwrap();
    let second = save(&library, &draft).unwrap();
    let third = save(&library, &draft).unwrap();

    assert_eq!(second.song, "Hymn 2.song");
    assert_eq!(third.song, "Hymn 3.song");
    assert_eq!(second.title, "Hymn");
    let listed: Vec<String> = library
        .list_songs()
        .unwrap()
        .songs
        .into_iter()
        .map(|song| song.song)
        .collect();
    assert_eq!(listed, ["Hymn 2.song", "Hymn 3.song"]);
    assert_eq!(library.list_songs().unwrap().songs[0], second);
}

#[test]
fn edits_to_the_draft_are_saved_and_changes_at_bars_become_sample_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("EG 1.wav"), RATE, 2 * 60 * RATE as usize);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let mut draft = probe_import(&source).unwrap();
    draft.song.title = "Way Maker".to_owned();
    draft.song.key = Some("E".to_owned());
    draft.song.bpm = 68.0;
    draft.song.changes = vec![
        MapChange {
            bar: 3,
            bpm: Some(136.0),
            time_signature: None,
        },
        MapChange {
            bar: 5,
            bpm: None,
            time_signature: Some(TimeSignature {
                numerator: 6,
                denominator: 8,
            }),
        },
    ];
    assert!(draft.song.set_part(0, Part::AcousticGuitar));
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    assert_eq!(entry.song, "Way Maker.song");
    assert_eq!((entry.key.as_deref(), entry.bpm), (Some("E"), 68.0));
    let json = fs::read_to_string(library_dir.join("Way Maker.song/song.json")).unwrap();
    let package = SongPackage::from_json(&json).unwrap();
    // 68 BPM: a beat is 42 352.94 samples, so bar 3 starts after 8 beats,
    // at 338 823.53 rounded to 338 824. 136 BPM: a beat is 21 176.47, so
    // bar 5 starts 8 beats later, at about 508 235.5, rounded to 508 236.
    let tempo: Vec<(u64, f64)> = package.tempo_map.iter().map(|p| (p.at, p.bpm)).collect();
    assert_eq!(tempo, [(0, 68.0), (338_824, 136.0)]);
    let meters: Vec<(u64, u32, u32)> = package
        .time_signature_map
        .iter()
        .map(|p| (p.at, p.numerator, p.denominator))
        .collect();
    assert_eq!(meters, [(0, 4, 4), (508_236, 6, 8)]);
    assert_eq!(package.stems[0].part, "acoustic-guitar");
}

#[test]
fn bad_edits_block_the_save_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let mut draft = probe_import(&source).unwrap();
    draft.song.title = " ".to_owned();
    draft.song.bpm = 0.0;
    let error = save(&Library::new(&library_dir), &draft).unwrap_err();

    let ImportError::Draft(errors) = &error else {
        panic!("{error}");
    };
    assert_eq!(
        errors,
        &[
            DraftError::EmptyTitle,
            DraftError::BpmOutOfRange { bar: 1, bpm: 0.0 }
        ]
    );
    assert_eq!(
        error.to_string(),
        "the song cannot be saved: the song needs a title; the tempo at bar 1 is 0 BPM; it must be from 1 to 999 BPM"
    );
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[test]
fn a_cancelled_import_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    for name in ["Drums.wav", "Bass.wav", "Keys.wav"] {
        wav(&source.join(name), RATE, 10 * RATE as usize);
    }
    fs::copy(fixture("tone.mp3"), source.join("Pad.mp3")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();
    let library = Library::new(&library_dir);
    let draft = probe_import(&source).unwrap();

    let cancel = AtomicBool::new(true);
    let error = library
        .save_import(&draft, &mut no_progress, &cancel)
        .unwrap_err();
    assert!(matches!(error, ImportError::Cancelled), "{error}");
    assert_eq!(tree(&library_dir), Vec::<String>::new());

    let cancel = AtomicBool::new(false);
    let mut reports = 0;
    let error = library
        .save_import(
            &draft,
            &mut |_| {
                reports += 1;
                cancel.store(true, Ordering::Relaxed);
            },
            &cancel,
        )
        .unwrap_err();
    assert!(matches!(error, ImportError::Cancelled), "{error}");
    assert!(reports >= 1);
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[test]
fn a_failure_in_the_middle_of_an_import_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Bass.wav"), RATE, 1_000);
    wav(&source.join("Drums.wav"), RATE, 1_000);
    fs::copy(fixture("tone.mp3"), source.join("Keys.mp3")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();
    let library = Library::new(&library_dir);
    let draft = probe_import(&source).unwrap();

    // A source file that changed after the probe fails the check.
    wav(&source.join("Drums.wav"), RATE, 2_000);
    let error = save(&library, &draft).unwrap_err();
    let ImportError::Stem { name, reason } = &error else {
        panic!("{error}");
    };
    assert_eq!(name, "Drums.wav");
    assert_eq!(
        reason,
        "the stem written is 2000 frames at 48000 Hz, but the file read was 1000 frames at 48000 Hz"
    );
    assert_eq!(tree(&library_dir), Vec::<String>::new());

    // A source file that is gone fails the copy.
    fs::remove_file(source.join("Drums.wav")).unwrap();
    let error = save(&library, &draft).unwrap_err();
    assert!(
        matches!(&error, ImportError::Stem { name, .. } if name == "Drums.wav"),
        "{error}"
    );
    assert_eq!(tree(&library_dir), Vec::<String>::new());

    // A damaged MP3 fails the transcoding.
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let mp3 = fs::read(source.join("Keys.mp3")).unwrap();
    fs::write(source.join("Keys.mp3"), &mp3[..mp3.len() / 2]).unwrap();
    let error = save(&library, &draft).unwrap_err();
    let ImportError::Stem { name, reason } = &error else {
        panic!("{error}");
    };
    assert_eq!(name, "Keys.mp3");
    assert!(reason.starts_with("the audio "), "{reason}");
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[test]
fn the_song_being_built_is_hidden_from_the_song_list_and_progress_reaches_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 3 * RATE as usize);
    fs::copy(fixture("tone.mp3"), source.join("Bass.mp3")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();
    let library = Library::new(&library_dir);
    let draft = probe_import(&source).unwrap();

    let mut updates = Vec::new();
    let entry = library
        .save_import(
            &draft,
            &mut |progress| {
                if updates.is_empty() {
                    let building = tree(&library_dir);
                    assert!(building[0].starts_with(".import-"), "{building:?}");
                    let listing = library.list_songs().unwrap();
                    assert!(listing.songs.is_empty() && listing.unreadable.is_empty());
                }
                updates.push(progress);
            },
            &AtomicBool::new(false),
        )
        .unwrap();

    let total = 3 * u64::from(RATE) + TONE_FRAMES;
    assert!(updates.iter().all(|update| update.total == total));
    assert!(updates.windows(2).all(|pair| pair[0].done <= pair[1].done));
    let last = updates.last().unwrap();
    assert_eq!((last.done, last.fraction()), (total, 1.0));
    for stem in 0..2 {
        let last = updates
            .iter()
            .rev()
            .find(|update| update.stem == stem)
            .unwrap();
        assert_eq!(last.stem_done, last.stem_frames);
    }
    assert_eq!(tree(&library_dir)[0], "Song.song");
    assert_eq!(library.list_songs().unwrap().songs, [entry]);
}

#[test]
fn a_saved_song_reopens_and_plays_identically() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    let drums = ramp(TONE_FRAMES as usize, 16);
    write_int(&source.join("Drums.wav"), 16, 2, RATE, &drums);
    fs::copy(fixture("tone.mp3"), source.join("Bass.mp3")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    let entry = save(&Library::new(&library_dir), &draft).unwrap();
    let folder = library_dir.join(&entry.song);
    let first = load_song(&folder).unwrap();
    let again = load_song(&folder).unwrap();

    assert_eq!(first, again);
    assert_eq!(first.song.length(), TONE_FRAMES);
    assert!(first.warnings.is_empty());
    assert_eq!(
        first.stems[0].samples,
        decoded_24_bit(&fixture("tone.mp3"), "mp3")
    );
    let drums: Vec<f32> = drums.iter().map(|&s| s as f32 / 32_768.0).collect();
    assert_eq!(first.stems[1].samples, drums);
}

/// Measures a release-build import of the folder `IMPORT_TIMING_SOURCE` into
/// the library folder `IMPORT_TIMING_LIBRARY` (both must exist):
/// `cargo test --release -p library --test import import_timing --
/// --ignored --nocapture`. The song it saves is left for a look and must be
/// removed by hand.
#[test]
#[ignore = "a measurement, not a check"]
fn import_timing() {
    let (Some(source), Some(library)) = (
        std::env::var_os("IMPORT_TIMING_SOURCE"),
        std::env::var_os("IMPORT_TIMING_LIBRARY"),
    ) else {
        eprintln!("set IMPORT_TIMING_SOURCE and IMPORT_TIMING_LIBRARY");
        return;
    };
    let started = std::time::Instant::now();
    let draft = probe_import(Path::new(&source)).unwrap();
    let probed = started.elapsed();
    let started = std::time::Instant::now();
    let entry = save(&Library::new(&library), &draft).unwrap();
    eprintln!(
        "{} stems ({} transcoded), {} frames: probe {probed:.2?}, save {:.2?}, into {}",
        draft.stems().len(),
        draft
            .stems()
            .iter()
            .filter(|stem| matches!(stem.action, StemAction::Transcode { .. }))
            .count(),
        entry.length_frames,
        started.elapsed(),
        entry.song
    );
}

#[test]
fn a_draft_whose_song_was_replaced_is_not_saved() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let mut draft = probe_import(&source).unwrap();
    draft.song = domain::SongDraft::new(
        "Other",
        vec![domain::DraftStem::new(
            "Bass.wav",
            StoredFormat::Wav,
            RATE,
            1_000,
        )],
    );

    let error = save(&Library::new(&library_dir), &draft).unwrap_err();
    assert!(matches!(error, ImportError::StemsChanged), "{error}");
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn an_audio_file_linked_from_outside_the_source_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    wav(&dir.path().join("outside.wav"), RATE, 1_000);
    fs::copy(fixture("tone.mp3"), dir.path().join("outside.mp3")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside.wav"), source.join("Link.wav")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside.mp3"), source.join("Link.mp3")).unwrap();

    let draft = probe_import(&source).unwrap();

    assert_eq!(names(&draft), ["Drums.wav"]);
    let refused: Vec<(&str, &str)> = draft
        .refused()
        .iter()
        .map(|file| (file.name.as_str(), file.reason.as_str()))
        .collect();
    assert_eq!(
        refused,
        [
            ("Link.mp3", "it resolves outside the source folder"),
            ("Link.wav", "it resolves outside the source folder"),
        ]
    );
}

/// `tests/fixtures/tone-32bit-stereo.flac` (see `tests/flac.rs`): the
/// decoder reads 32-bit FLAC wrong, so the import refuses it rather than
/// transcode it, whatever its name and with or without an ID3 tag in front.
#[test]
fn a_flac_deeper_than_24_bits_is_refused_not_transcoded() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let deep = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone-32bit-stereo.flac"),
    )
    .unwrap();
    fs::write(source.join("Deep.flac"), &deep).unwrap();
    fs::write(source.join("Renamed.wav"), &deep).unwrap();
    let mut tagged = id3_tag();
    tagged.extend(&deep);
    fs::write(source.join("Tagged.flac"), tagged).unwrap();

    let draft = probe_import(&source).unwrap();

    assert_eq!(names(&draft), ["Drums.wav"]);
    let refused: Vec<(&str, &str)> = draft
        .refused()
        .iter()
        .map(|file| (file.name.as_str(), file.reason.as_str()))
        .collect();
    let reason = "it cannot be decoded: it is 32-bit FLAC, and FLAC is decoded up to 24-bit";
    assert_eq!(
        refused,
        [
            ("Deep.flac", reason),
            ("Renamed.wav", reason),
            ("Tagged.flac", reason),
        ]
    );
}

/// Stereo high-passed white noise near full scale, with the decaying
/// envelope of a crash cymbal every two seconds: what a lossless encoder
/// cannot compress.
fn crash_noise(frames: usize) -> Vec<f64> {
    let mut state = 1u64;
    let mut previous = [0.0f64; 2];
    let mut samples = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let t = frame as f64 / f64::from(RATE);
        let envelope = (-2.0 * (t % 2.0)).exp();
        for last in &mut previous {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let white = (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
            let high = (white - *last) / 2.0;
            *last = white;
            samples.push((high * 0.8 * envelope).clamp(-1.0, 1.0));
        }
    }
    samples
}

#[test]
fn loud_noise_is_stored_no_larger_than_its_raw_24_bit_samples_and_plays() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    fs::create_dir(&source).unwrap();
    let frames = 6 * RATE as usize;
    let noise = crash_noise(frames);
    fs::write(source.join("Crash.wav"), float64_wav(2, RATE, &noise)).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    assert!(matches!(
        draft.stems()[0].action,
        StemAction::Transcode { .. }
    ));
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let flac = library_dir.join(&entry.song).join("stems/crash.flac");
    let raw = (frames * 2 * 3) as u64;
    let size = fs::metadata(&flac).unwrap().len();
    assert!(
        size <= raw + raw / 100,
        "{size} bytes of FLAC for {raw} bytes of 24-bit samples"
    );
    // STREAMINFO's frame sizes bound the frames written, and no frame is
    // larger than 4 096 stereo 24-bit samples stored verbatim (24 576 bytes
    // and a few bytes of headers).
    let bytes = fs::read(&flac).unwrap();
    let be = |range: std::ops::Range<usize>| {
        bytes[range]
            .iter()
            .fold(0u64, |value, &byte| value << 8 | u64::from(byte))
    };
    let (min_frame, max_frame) = (be(12..15), be(15..18));
    let blocks = frames.div_ceil(4_096) as u64;
    let audio = size - 42;
    assert!(max_frame <= 24_576 + 32, "{max_frame}");
    assert!(min_frame * blocks <= audio && audio <= max_frame * blocks);
    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    assert_eq!(loaded.stems[0].frames, frames as u64);
    for (got, want) in loaded.stems[0].samples.iter().zip(&noise) {
        assert!((f64::from(*got) - want).abs() <= 0.5 / 8_388_608.0 + 1e-12);
    }
}

#[test]
fn a_stem_that_fails_the_check_before_saving_is_named_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Bass.wav"), RATE, 1_000);
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();
    let draft = probe_import(&source).unwrap();

    fs::write(source.join("Drums.wav"), b"no longer a WAV file").unwrap();
    let error = save(&Library::new(&library_dir), &draft).unwrap_err();

    let ImportError::Stem { name, reason } = &error else {
        panic!("{error}");
    };
    assert_eq!(name, "Drums.wav");
    assert!(
        reason.contains("cannot be read: Ill-formed WAVE file: no RIFF tag found"),
        "{reason}"
    );
    assert_eq!(tree(&library_dir), Vec::<String>::new());
}

#[test]
fn an_apple_m4a_drops_the_delay_and_padding_its_itunsmpb_tag_records() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    fs::create_dir(&source).unwrap();
    fs::copy(fixture("apple.m4a"), source.join("Pad.m4a")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    assert_eq!(draft.song.stems()[0].length_frames, 60_010);
    assert!(!draft.stems()[0].estimated_length);
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    let samples = &loaded.stems[0].samples;
    assert_eq!(samples.len(), 60_010, "mono");
    let peak = (0..samples.len())
        .max_by(|&a, &b| samples[a].abs().total_cmp(&samples[b].abs()))
        .unwrap();
    assert!(
        peak.abs_diff(IMPULSE) <= 3,
        "the impulse is at frame {peak}"
    );
}

#[test]
fn an_mp3_without_a_header_is_saved_at_the_length_its_decoder_gives() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    fs::create_dir(&source).unwrap();
    fs::copy(fixture("noxing.mp3"), source.join("Bass.mp3")).unwrap();
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();
    assert!(draft.stems()[0].estimated_length);
    let entry = save(&Library::new(&library_dir), &draft).unwrap();

    let decoded = decoded_24_bit(&fixture("noxing.mp3"), "mp3");
    assert_eq!(decoded.len(), 57_600, "50 packets of 1 152 frames, mono");
    assert_eq!(entry.length_frames, 57_600);
    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    assert_eq!(loaded.stems[0].samples, decoded);
}

/// Cuts the file at `path` to half its size.
fn cut_in_half(path: &Path) {
    let len = fs::metadata(path).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len / 2)
        .unwrap();
}

#[test]
fn a_wav_or_flac_cut_short_is_refused_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    wav(&source.join("Bass.wav"), RATE, 20_000);
    write_flac(&source.join("Keys.flac"), 24, 2, RATE, &ramp(20_000, 24));
    let int8: Vec<i32> = (0..20_000).map(|i| (i * 37) % 256 - 128).collect();
    write_int(&source.join("Shaker.wav"), 8, 1, RATE, &int8);
    for name in ["Bass.wav", "Keys.flac", "Shaker.wav"] {
        cut_in_half(&source.join(name));
    }

    let draft = probe_import(&source).unwrap();

    assert_eq!(names(&draft), ["Drums.wav"]);
    let refused: Vec<(&str, &str)> = draft
        .refused()
        .iter()
        .map(|file| (file.name.as_str(), file.reason.as_str()))
        .collect();
    assert_eq!(refused.len(), 3, "{refused:?}");
    assert_eq!(
        refused[0],
        (
            "Bass.wav",
            "it ends before the 20000 frames its header declares"
        )
    );
    assert_eq!(refused[1].0, "Keys.flac");
    assert!(
        refused[1]
            .1
            .starts_with("it ends before the 20000 frames its header declares"),
        "{}",
        refused[1].1
    );
    assert_eq!(
        refused[2],
        (
            "Shaker.wav",
            "it ends before the 20000 frames its header declares"
        )
    );
}

#[test]
fn a_wav_or_flac_cut_short_after_the_probe_is_not_saved() {
    for name in ["Bass.wav", "Keys.flac"] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Song");
        wav(&source.join("Bass.wav"), RATE, 20_000);
        write_flac(&source.join("Keys.flac"), 24, 2, RATE, &ramp(20_000, 24));
        let library_dir = dir.path().join("Library");
        fs::create_dir(&library_dir).unwrap();
        let draft = probe_import(&source).unwrap();

        cut_in_half(&source.join(name));
        let error = save(&Library::new(&library_dir), &draft).unwrap_err();

        let ImportError::Stem { name: stem, reason } = &error else {
            panic!("{error}");
        };
        assert_eq!(stem, name);
        assert!(
            reason.starts_with("the stem written does not read to its end: "),
            "{reason}"
        );
        assert_eq!(tree(&library_dir), Vec::<String>::new(), "{name}");
    }
}

/// Sets the modification time of `path` to `ago` before now, a folder's
/// after everything in it, since adding to a folder changes its time.
fn set_age(path: &Path, ago: Duration) {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            set_age(&entry.unwrap().path(), ago);
        }
    }
    open_for_times(path)
        .set_modified(SystemTime::now() - ago)
        .unwrap();
}

#[cfg(windows)]
fn open_for_times(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_BACKUP_SEMANTICS lets a folder open as a file.
    fs::OpenOptions::new()
        .write(true)
        .custom_flags(0x0200_0000)
        .open(path)
        .unwrap()
}

#[cfg(not(windows))]
fn open_for_times(path: &Path) -> fs::File {
    fs::File::open(path).unwrap()
}

/// A hidden build folder named `name` in `library`, with a stem half
/// written in it, as an import cut off would leave it.
fn build_folder(library: &Path, name: &str) -> PathBuf {
    let folder = library.join(name);
    fs::create_dir_all(folder.join("stems")).unwrap();
    fs::write(folder.join("stems/drums.flac"), b"half a stem").unwrap();
    folder
}

#[test]
fn an_import_removes_what_imports_cut_off_long_ago_left_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Song");
    wav(&source.join("Drums.wav"), RATE, 1_000);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();
    let ours = std::process::id();
    let other = ours.wrapping_add(1);
    let long_ago = Duration::from_secs(2 * 3_600);

    // Left by another process's import over an hour ago (a crash, a kill):
    // removed.
    let stale = build_folder(&library_dir, &format!(".import-{other}-0.tmp"));
    set_age(&stale, long_ago);
    // Another process's import, still writing or only just stopped: kept.
    build_folder(&library_dir, &format!(".import-{other}-1.tmp"));
    let writing = build_folder(&library_dir, &format!(".import-{other}-2.tmp"));
    set_age(&writing, long_ago);
    open_for_times(&writing.join("stems/drums.flac"))
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();
    // This process's own, however old: kept.
    let own = build_folder(&library_dir, &format!(".import-{ours}-7.tmp"));
    set_age(&own, long_ago);
    // Names the import never makes, however old: kept.
    let others = [
        format!(".import-{other}-3.tmp.bak"),
        format!(".import-{other}-3"),
        format!("import-{other}-3.tmp"),
        format!(".import-0{other}-3.tmp"),
        format!(".import-{other}-03.tmp"),
        ".import--3.tmp".to_owned(),
        format!(".import-{other}-.tmp"),
        format!(".import-x{other}-3.tmp"),
        format!(".Import-{other}-3.tmp"),
    ];
    for name in &others {
        let folder = build_folder(&library_dir, name);
        set_age(&folder, long_ago);
    }
    // A file with a build folder's name: kept.
    let file = library_dir.join(format!(".import-{other}-4.tmp"));
    fs::write(&file, b"not a folder").unwrap();
    set_age(&file, long_ago);
    // A link with a build folder's name: kept, and so is what it links to.
    #[cfg(unix)]
    {
        let target = dir.path().join("Elsewhere");
        build_folder(&target, "inside");
        set_age(&target, long_ago);
        std::os::unix::fs::symlink(&target, library_dir.join(format!(".import-{other}-5.tmp")))
            .unwrap();
    }
    let before: Vec<String> = tree(&library_dir);

    let entry = save(&Library::new(&library_dir), &probe_import(&source).unwrap()).unwrap();

    let stale_name = format!(".import-{other}-0.tmp");
    let mut expected: Vec<String> = before
        .into_iter()
        .filter(|path| !path.starts_with(&stale_name))
        .collect();
    expected.extend(
        tree(&library_dir.join(&entry.song))
            .into_iter()
            .map(|path| format!("{}/{path}", entry.song)),
    );
    expected.push(entry.song.clone());
    expected.sort();
    assert_eq!(tree(&library_dir), expected);
    #[cfg(unix)]
    assert!(
        dir.path()
            .join("Elsewhere/inside/stems/drums.flac")
            .exists()
    );
}

/// `apple.m4a` with `tail` appended after its audio, as a damaged file
/// would end, in a source folder of its own.
fn apple_with_tail(dir: &Path, tail: &[u8]) -> PathBuf {
    let source = dir.join("Song");
    fs::create_dir_all(&source).unwrap();
    let mut bytes = fs::read(fixture("apple.m4a")).unwrap();
    bytes.extend_from_slice(tail);
    fs::write(source.join("Pad.m4a"), bytes).unwrap();
    source
}

#[test]
fn an_m4a_box_claiming_more_than_the_file_ends_the_box_walk() {
    let dir = tempfile::tempdir().unwrap();
    // A box with a 64-bit size near u64::MAX: adding it to its offset
    // overflows.
    let mut tail = Vec::new();
    tail.extend_from_slice(&1u32.to_be_bytes());
    tail.extend_from_slice(b"free");
    tail.extend_from_slice(&(u64::MAX - 7).to_be_bytes());
    let source = apple_with_tail(dir.path(), &tail);

    let draft = probe_import(&source).unwrap();

    assert!(draft.refused().is_empty(), "{:?}", draft.refused());
    let stem = &draft.stems()[0];
    assert!(!stem.estimated_length, "the iTunSMPB tag before it is read");
    assert_eq!(draft.song.stems()[0].length_frames, 60_010);
}

#[test]
fn an_m4a_whose_boxes_cannot_be_read_imports_untrimmed_at_its_decoded_length() {
    let dir = tempfile::tempdir().unwrap();
    // A box whose 64-bit size the file ends before.
    let mut tail = Vec::new();
    tail.extend_from_slice(&1u32.to_be_bytes());
    tail.extend_from_slice(b"free");
    let source = apple_with_tail(dir.path(), &tail);
    let library_dir = dir.path().join("Library");
    fs::create_dir(&library_dir).unwrap();

    let draft = probe_import(&source).unwrap();

    assert!(draft.refused().is_empty(), "{:?}", draft.refused());
    assert!(draft.stems()[0].estimated_length);
    let entry = save(&Library::new(&library_dir), &draft).unwrap();
    let loaded = load_song(&library_dir.join(&entry.song)).unwrap();
    assert!(
        loaded.stems[0].samples.len() >= 60_010,
        "untrimmed: {} frames",
        loaded.stems[0].samples.len()
    );
}
