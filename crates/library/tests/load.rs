//! S5: reading song folders and WAV stems, writing the rendered WAV.

mod common;

use std::fs;

use common::{manifest, write_float, write_int, write_manifest};
use domain::{StemRole, ValidationError, ValidationWarning};
use library::{LoadError, SongProblem, WriteError, load_song, write_wav_stereo_f32};

const RATE: u32 = 1_000;

fn problems(error: LoadError) -> Vec<SongProblem> {
    match error {
        LoadError::Invalid(problems) => problems,
        other => panic!("expected an invalid package, got {other}"),
    }
}

#[test]
fn decodes_integer_and_float_stems_to_f32() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("int16", "stems/int16.wav", StemRole::Track),
                ("int24", "stems/int24.wav", StemRole::Track),
                ("int32", "stems/int32.wav", StemRole::Click),
                ("float", "stems/float.wav", StemRole::Guide),
            ],
        ),
    );
    write_int(
        &path.join("stems/int16.wav"),
        16,
        1,
        RATE,
        &[0, 16_384, -32_768, 32_767],
    );
    write_int(
        &path.join("stems/int24.wav"),
        24,
        2,
        RATE,
        &[0, 4_194_304, -8_388_608, 8_388_607, -4_194_304, 1, 0, 0],
    );
    write_int(
        &path.join("stems/int32.wav"),
        32,
        1,
        RATE,
        &[0, 1 << 30, i32::MIN, i32::MAX],
    );
    write_float(
        &path.join("stems/float.wav"),
        2,
        RATE,
        &[0.0, 0.25, -1.0, 0.75, 0.5, -0.5, 1.0, -0.125],
    );

    let loaded = load_song(path).expect("song loads");

    assert_eq!(loaded.song.title(), "Library Test");
    assert_eq!(loaded.song.length(), 4);
    assert!(loaded.warnings.is_empty());
    let ids: Vec<&str> = loaded.stems.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["int16", "int24", "int32", "float"], "package order");
    let shapes: Vec<(StemRole, u16, u64)> = loaded
        .stems
        .iter()
        .map(|s| (s.role, s.channels, s.frames))
        .collect();
    assert_eq!(
        shapes,
        [
            (StemRole::Track, 1, 4),
            (StemRole::Track, 2, 4),
            (StemRole::Click, 1, 4),
            (StemRole::Guide, 2, 4),
        ]
    );
    assert_eq!(
        loaded.stems[0].samples,
        [0.0, 0.5, -1.0, 32_767.0 / 32_768.0]
    );
    assert_eq!(
        loaded.stems[1].samples,
        [
            0.0,
            0.5,
            -1.0,
            8_388_607.0 / 8_388_608.0,
            -0.5,
            1.0 / 8_388_608.0,
            0.0,
            0.0
        ]
    );
    let int32_max = (f64::from(i32::MAX) / 2_147_483_648.0) as f32;
    assert_eq!(loaded.stems[2].samples, [0.0, 0.5, -1.0, int32_max]);
    assert_eq!(
        loaded.stems[3].samples,
        [0.0, 0.25, -1.0, 0.75, 0.5, -0.5, 1.0, -0.125]
    );
}

#[test]
fn unsupported_or_unreadable_stems_are_errors_naming_the_stem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("eight", "stems/eight.wav", StemRole::Track),
                ("surround", "stems/surround.wav", StemRole::Track),
                ("text", "stems/text.wav", StemRole::Track),
                ("missing", "stems/missing.wav", StemRole::Track),
                ("good", "stems/good.wav", StemRole::Track),
            ],
        ),
    );
    write_int(&path.join("stems/eight.wav"), 8, 1, RATE, &[0, 1, 2, 3]);
    write_int(&path.join("stems/surround.wav"), 16, 3, RATE, &[0; 12]);
    fs::write(path.join("stems/text.wav"), "not a wav file").unwrap();
    write_int(&path.join("stems/good.wav"), 16, 1, RATE, &[0; 4]);

    let problems = problems(load_song(path).unwrap_err());

    let ids: Vec<&str> = problems
        .iter()
        .map(|problem| match problem {
            SongProblem::UnsupportedStemFormat { id, .. } => id.as_str(),
            SongProblem::StemUnreadable { id, .. } => id.as_str(),
            other => panic!("unexpected problem {other}"),
        })
        .collect();
    assert_eq!(
        ids,
        ["eight", "surround", "text", "missing"],
        "one problem per bad stem, no MissingStemInfo duplicate"
    );
    let messages: Vec<String> = problems.iter().map(ToString::to_string).collect();
    assert!(messages[0].contains("\"eight\"") && messages[0].contains("8-bit"));
    assert!(messages[1].contains("\"surround\"") && messages[1].contains("3 channels"));
    assert!(messages[2].contains("\"text\"") && messages[2].contains("stems/text.wav"));
    assert!(messages[3].contains("\"missing\"") && messages[3].contains("stems/missing.wav"));
}

#[cfg(unix)]
#[test]
fn stem_paths_escaping_the_song_folder_through_a_symlink_are_rejected() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    write_int(&outside.path().join("secret.wav"), 16, 1, RATE, &[0; 4]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("escape", "stems/escape.wav", StemRole::Track),
                ("linked", "linked/inside.wav", StemRole::Track),
            ],
        ),
    );
    fs::create_dir_all(path.join("stems")).unwrap();
    symlink(
        outside.path().join("secret.wav"),
        path.join("stems/escape.wav"),
    )
    .unwrap();
    write_int(&path.join("real/inside.wav"), 16, 1, RATE, &[0; 4]);
    symlink(path.join("real"), path.join("linked")).unwrap();

    let problems = problems(load_song(path).unwrap_err());

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        matches!(
            &problems[0],
            SongProblem::StemOutsideFolder { id, file } if id == "escape" && file == "stems/escape.wav"
        ),
        "{problems:?}"
    );
    assert!(problems[0].to_string().contains("outside the song folder"));

    // A symlink that stays inside the folder is fine.
    write_manifest(
        path,
        &manifest(RATE, &[("linked", "linked/inside.wav", StemRole::Track)]),
    );
    assert!(load_song(path).is_ok());
}

#[test]
fn lexically_invalid_stem_paths_report_the_validation_error_once() {
    let outside = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let parent_file = format!(
        "../{}/a.wav",
        outside.path().file_name().unwrap().to_str().unwrap()
    );
    write_int(&outside.path().join("a.wav"), 16, 1, RATE, &[0; 4]);
    write_int(&path.join("good.wav"), 16, 1, RATE, &[0; 4]);
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("parent", &parent_file, StemRole::Track),
                ("good", "good.wav", StemRole::Track),
            ],
        ),
    );

    let problems = problems(load_song(path).unwrap_err());

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        matches!(
            &problems[0],
            SongProblem::Validation(ValidationError::InvalidStemPath { id, .. }) if id == "parent"
        ),
        "{problems:?}"
    );
}

#[test]
fn every_validation_error_is_returned_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let mut package = manifest(
        RATE,
        &[
            ("drums", "drums.wav", StemRole::Track),
            ("bass", "bass.wav", StemRole::Track),
        ],
    );
    package.arrangements[0].id = "live".to_owned();
    package.arrangements[0].sections.push("bridge".to_owned());
    write_manifest(path, &package);
    write_int(&path.join("drums.wav"), 16, 1, RATE, &[0; 4]);
    write_int(&path.join("bass.wav"), 16, 1, 2 * RATE, &[0; 4]);

    let problems = problems(load_song(path).unwrap_err());

    assert_eq!(
        problems,
        [
            SongProblem::Validation(ValidationError::StemSampleRateMismatch {
                id: "bass".into(),
                found: 2 * RATE,
                expected: RATE
            }),
            SongProblem::Validation(ValidationError::MissingDefaultArrangement),
            SongProblem::Validation(ValidationError::UnknownSectionReference {
                arrangement: "live".into(),
                section: "bridge".into()
            }),
        ]
    );
}

#[test]
fn stem_length_warnings_are_passed_through() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("drums", "drums.wav", StemRole::Track),
                ("bass", "bass.wav", StemRole::Track),
            ],
        ),
    );
    write_int(&path.join("drums.wav"), 16, 1, RATE, &[0; 2_500]);
    write_int(&path.join("bass.wav"), 16, 1, RATE, &[0; 1_000]);

    let loaded = load_song(path).expect("song loads with a warning");

    assert_eq!(
        loaded.warnings,
        [ValidationWarning::StemLengthsDiffer {
            shortest: "bass".into(),
            shortest_length: 1_000,
            longest: "drums".into(),
            longest_length: 2_500
        }]
    );
}

#[test]
fn a_missing_or_malformed_manifest_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = load_song(dir.path()).unwrap_err();
    assert!(matches!(error, LoadError::ReadManifest { .. }), "{error}");
    assert!(error.to_string().contains("song.json"));

    fs::write(dir.path().join("song.json"), "{ not json").unwrap();
    let error = load_song(dir.path()).unwrap_err();
    assert!(matches!(error, LoadError::Manifest(_)), "{error}");
    assert!(error.to_string().contains("song.json"));
}

#[test]
fn writes_stereo_32_bit_float_wav() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.wav");
    let samples = [0.0, -0.0, 0.5, -1.0, 1.5, f32::MIN_POSITIVE];

    write_wav_stereo_f32(&file, 44_100, &samples).expect("writes");

    let mut reader = hound::WavReader::open(&file).unwrap();
    let spec = reader.spec();
    assert_eq!(
        (
            spec.channels,
            spec.sample_rate,
            spec.bits_per_sample,
            spec.sample_format
        ),
        (2, 44_100, 32, hound::SampleFormat::Float)
    );
    let read: Vec<u32> = reader
        .samples::<f32>()
        .map(|sample| sample.unwrap().to_bits())
        .collect();
    let written: Vec<u32> = samples.iter().map(|sample| sample.to_bits()).collect();
    assert_eq!(read, written);

    assert!(matches!(
        write_wav_stereo_f32(&file, 44_100, &[0.0; 3]),
        Err(WriteError::NotStereo { samples: 3 })
    ));
    let missing = dir.path().join("no/such/dir/out.wav");
    let error = write_wav_stereo_f32(&missing, 44_100, &samples).unwrap_err();
    assert!(error.to_string().contains("out.wav"), "{error}");
}

/// Runs `load_song` on another thread and gives up after five seconds, so a
/// loader that opens a FIFO (and blocks) fails the test instead of hanging it.
#[cfg(unix)]
fn load_within_five_seconds(path: &std::path::Path) -> Result<library::LoadedSong, LoadError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = path.to_owned();
    std::thread::spawn(move || {
        let _ = sender.send(load_song(&path));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("load_song returns without opening a FIFO")
}

#[cfg(unix)]
fn mkfifo(path: &std::path::Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "mkfifo {}", path.display());
}

#[cfg(unix)]
#[test]
fn escaping_stem_paths_are_rejected_without_opening_the_target() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("outside.fifo");
    mkfifo(&fifo);
    let song = dir.path().join("song");
    fs::create_dir_all(song.join("stems")).unwrap();
    symlink(&fifo, song.join("stems/link.wav")).unwrap();
    let absolute = fifo.to_str().unwrap().to_owned();
    write_manifest(
        &song,
        &manifest(
            RATE,
            &[
                ("parent", "../outside.fifo", StemRole::Track),
                ("absolute", &absolute, StemRole::Track),
                ("link", "stems/link.wav", StemRole::Track),
            ],
        ),
    );

    let problems = problems(load_within_five_seconds(&song).unwrap_err());

    let mut found: Vec<(String, &str)> = problems
        .iter()
        .map(|problem| match problem {
            SongProblem::Validation(ValidationError::InvalidStemPath { id, .. }) => {
                (id.clone(), "invalid path")
            }
            SongProblem::StemOutsideFolder { id, .. } => (id.clone(), "outside"),
            other => panic!("unexpected problem {other}"),
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            ("absolute".to_owned(), "invalid path"),
            ("link".to_owned(), "outside"),
            ("parent".to_owned(), "invalid path"),
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_fifo_inside_the_song_folder_is_rejected_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    mkfifo(&path.join("pipe.wav"));
    write_manifest(
        path,
        &manifest(RATE, &[("pipe", "pipe.wav", StemRole::Track)]),
    );

    let problems = problems(load_within_five_seconds(path).unwrap_err());

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        matches!(&problems[0], SongProblem::StemUnreadable { id, reason, .. }
            if id == "pipe" && reason.contains("not a regular file")),
        "{problems:?}"
    );
}

/// The messages a broken WAV stem gives stay word for word as they were
/// before FLAC stems: unreadable, unsupported, and a header claiming more
/// samples than the file holds, loaded and streamed.
#[test]
fn wav_stem_messages_stay_word_for_word() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    write_manifest(
        path,
        &manifest(
            RATE,
            &[
                ("text", "text.wav", StemRole::Track),
                ("eight", "eight.wav", StemRole::Track),
            ],
        ),
    );
    fs::write(path.join("text.wav"), "not a wav file").unwrap();
    write_int(&path.join("eight.wav"), 8, 1, RATE, &[0; 4]);

    let messages: Vec<String> = problems(load_song(path).unwrap_err())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        messages,
        [
            "stem \"text\" file \"text.wav\" cannot be read: Ill-formed WAVE file: no RIFF tag found",
            "stem \"eight\" file \"eight.wav\" is 8-bit integer, 1 channel; supported stems are 16-, 24- or 32-bit integer or 32-bit float WAV, mono or stereo",
        ]
    );

    write_manifest(
        path,
        &manifest(RATE, &[("short", "short.wav", StemRole::Track)]),
    );
    let file = path.join("short.wav");
    write_int(&file, 16, 1, RATE, &[0; 1_000]);
    let mut bytes = fs::read(&file).unwrap();
    // 16-bit mono uses the 44-byte PCM header: the data size is at 40..44.
    bytes[40..44].copy_from_slice(&4_000u32.to_le_bytes());
    fs::write(&file, bytes).unwrap();

    assert_eq!(
        load_song(path).unwrap_err().to_string(),
        "stem \"short\" file \"short.wav\" cannot be decoded: Failed to read enough bytes."
    );
    let streamed = library::open_streaming(path, &library::StreamConfig::default())
        .expect_err("the header claims more samples than the file holds");
    assert_eq!(
        streamed.to_string(),
        "stem \"short\" file \"short.wav\" cannot be decoded: Ill-formed WAVE file: the file holds fewer samples than its header declares"
    );
}
