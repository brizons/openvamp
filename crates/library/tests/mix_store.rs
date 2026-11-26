//! S1: a song's `mix.json`, read and written beside its `song.json` (F7.5,
//! L2).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{manifest, write_manifest};
use domain::{
    ArrangementMix, MixFile, MixValidationError, MixerFileError, Song, StemInfo, StemMix, StemRole,
    Volume, validate,
};
use library::{
    MANIFEST_FILE, MAX_SETTINGS_FILE_BYTES, MIX_FILE, SettingsError, read_mix, write_mix,
};
use tempfile::TempDir;

/// A song folder with a `song.json` (stems `drums` and `click`, the Default
/// arrangement) and the song it validates to; no audio is needed.
fn song_folder() -> (TempDir, PathBuf, Song) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Example.song");
    fs::create_dir(&folder).unwrap();
    let package = manifest(
        48_000,
        &[
            ("drums", "stems/drums.wav", StemRole::Track),
            ("click", "stems/click.wav", StemRole::Click),
        ],
    );
    write_manifest(&folder, &package);
    let infos = ["drums", "click"].map(|id| StemInfo {
        id: id.to_owned(),
        sample_rate: 48_000,
        length_samples: 48_000,
    });
    let song = validate(&package, &infos).expect("valid").song;
    (dir, folder, song)
}

fn a_mix() -> MixFile {
    let mut default = ArrangementMix::default();
    default.set_stem(
        "drums",
        StemMix {
            volume: Volume::from_db(-4.5),
            solo: true,
            ..StemMix::default()
        },
    );
    default.set_stem(
        "click",
        StemMix {
            volume: Volume::SILENT,
            ..StemMix::default()
        },
    );
    let mut mix = MixFile::new();
    mix.set_arrangement("default", default);
    mix
}

/// The names in `folder`, sorted: a leftover temporary file shows here.
fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_mix_round_trips_beside_song_json_without_touching_it() {
    let (_dir, folder, song) = song_folder();
    let manifest_before = fs::read(folder.join(MANIFEST_FILE)).unwrap();

    write_mix(&folder, &a_mix()).expect("written");
    assert_eq!(
        entries(&folder),
        [MIX_FILE, MANIFEST_FILE],
        "no temporary file left"
    );
    assert_eq!(
        fs::read(folder.join(MANIFEST_FILE)).unwrap(),
        manifest_before
    );
    let written = fs::read_to_string(folder.join(MIX_FILE)).unwrap();
    assert_eq!(MixFile::from_json(&written).unwrap(), a_mix());

    let read = read_mix(&folder, &song);
    assert_eq!(read.mix, a_mix());
    assert!(read.error.is_none(), "{:?}", read.error);
    assert!(read.problems.is_empty());

    // Writing again replaces the file in place.
    write_mix(&folder, &MixFile::new()).expect("written");
    assert_eq!(read_mix(&folder, &song).mix, MixFile::new());
    assert_eq!(entries(&folder), [MIX_FILE, MANIFEST_FILE]);
    assert_eq!(
        fs::read(folder.join(MANIFEST_FILE)).unwrap(),
        manifest_before
    );
}

#[test]
fn a_song_without_mix_json_plays_with_the_defaults() {
    let (_dir, folder, song) = song_folder();
    let read = read_mix(&folder, &song);
    assert_eq!(read.mix, MixFile::new());
    assert!(read.error.is_none() && read.problems.is_empty());
    assert_eq!(entries(&folder), [MANIFEST_FILE], "reading writes nothing");
}

#[test]
fn a_mix_that_breaks_rules_is_used_and_its_problems_reported() {
    let (_dir, folder, song) = song_folder();
    let mut mix = a_mix();
    let mut acoustic = ArrangementMix::default();
    acoustic.set_stem(
        "piano",
        StemMix {
            mute: true,
            ..StemMix::default()
        },
    );
    mix.set_arrangement("acoustic", acoustic);
    write_mix(&folder, &mix).unwrap();

    let read = read_mix(&folder, &song);
    assert_eq!(
        read.mix, mix,
        "kept: unknown entries are ignored when playing"
    );
    assert!(read.error.is_none());
    assert_eq!(
        read.problems,
        [
            MixValidationError::UnknownArrangement {
                arrangement: "acoustic".to_owned()
            },
            MixValidationError::UnknownStem {
                arrangement: "acoustic".to_owned(),
                stem: "piano".to_owned()
            },
        ]
    );
}

#[test]
fn a_malformed_mix_is_reported_and_left_until_the_mix_changes() {
    let (_dir, folder, song) = song_folder();
    let manifest_before = fs::read(folder.join(MANIFEST_FILE)).unwrap();
    fs::write(folder.join(MIX_FILE), "{ not json").unwrap();

    let read = read_mix(&folder, &song);
    assert_eq!(read.mix, MixFile::new(), "the song plays with the defaults");
    match &read.error {
        Some(SettingsError::Format {
            path,
            source: MixerFileError::Json { .. },
        }) => assert!(path.ends_with(MIX_FILE)),
        other => panic!("expected a format error, got {other:?}"),
    }
    assert!(read.error.unwrap().to_string().contains(MIX_FILE));
    assert_eq!(
        fs::read_to_string(folder.join(MIX_FILE)).unwrap(),
        "{ not json",
        "reading leaves the file"
    );

    // When the user changes the mix, the new one replaces it.
    write_mix(&folder, &a_mix()).expect("written");
    assert_eq!(read_mix(&folder, &song).mix, a_mix());
    assert_eq!(
        fs::read(folder.join(MANIFEST_FILE)).unwrap(),
        manifest_before
    );
}

#[test]
fn a_newer_mix_is_reported_and_never_overwritten() {
    let (_dir, folder, song) = song_folder();
    let newer = r#"{ "formatVersion": 2, "arrangements": { "default": { "levels": [] } } }"#;
    fs::write(folder.join(MIX_FILE), newer).unwrap();

    let read = read_mix(&folder, &song);
    assert_eq!(read.mix, MixFile::new());
    assert!(
        matches!(
            read.error,
            Some(SettingsError::Format {
                source: MixerFileError::UnsupportedFormatVersion { found: 2, .. },
                ..
            })
        ),
        "{:?}",
        read.error
    );

    let error = write_mix(&folder, &a_mix()).expect_err("refused");
    assert!(
        matches!(
            error,
            SettingsError::NewerVersion {
                found: 2,
                supported: 1,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(fs::read_to_string(folder.join(MIX_FILE)).unwrap(), newer);
    assert_eq!(entries(&folder), [MIX_FILE, MANIFEST_FILE]);
}

#[test]
fn a_mix_larger_than_the_limit_is_not_read() {
    let (_dir, folder, song) = song_folder();
    let large = format!(
        r#"{{ "formatVersion": 1, "pad": "{}" }}"#,
        "x".repeat(MAX_SETTINGS_FILE_BYTES as usize)
    );
    fs::write(folder.join(MIX_FILE), &large).unwrap();
    let read = read_mix(&folder, &song);
    assert_eq!(read.mix, MixFile::new());
    assert!(
        matches!(read.error, Some(SettingsError::TooLarge { max_bytes, .. }) if max_bytes == MAX_SETTINGS_FILE_BYTES),
        "{:?}",
        read.error
    );
}

/// A UTF-16 file: a byte-order mark, then `{"formatVersion":2}` in UTF-16LE.
fn utf16_file() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(r#"{"formatVersion":2}"#.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

#[test]
fn a_mix_that_is_not_utf8_is_reported_and_replaced_when_the_mix_changes() {
    let (_dir, folder, song) = song_folder();
    fs::write(folder.join(MIX_FILE), utf16_file()).unwrap();

    let read = read_mix(&folder, &song);
    assert_eq!(read.mix, MixFile::new(), "the song plays with the defaults");
    assert!(
        matches!(&read.error, Some(SettingsError::NotText { path }) if path.ends_with(MIX_FILE)),
        "{:?}",
        read.error
    );
    assert_eq!(fs::read(folder.join(MIX_FILE)).unwrap(), utf16_file());

    // JSON is UTF-8, so no version of this app wrote it: a save replaces it.
    write_mix(&folder, &a_mix()).expect("replaced");
    assert_eq!(read_mix(&folder, &song).mix, a_mix());
    assert_eq!(entries(&folder), [MIX_FILE, MANIFEST_FILE]);
}

#[test]
fn a_failed_write_leaves_no_temporary_file_and_the_folder_as_it_was() {
    let (_dir, folder, _song) = song_folder();
    let manifest_before = fs::read(folder.join(MANIFEST_FILE)).unwrap();
    // A folder where mix.json should be: the rename over it fails.
    fs::create_dir(folder.join(MIX_FILE)).unwrap();
    fs::write(folder.join(MIX_FILE).join("keep"), "kept").unwrap();

    let error = write_mix(&folder, &a_mix()).expect_err("cannot replace a folder");
    assert!(matches!(error, SettingsError::Write { .. }), "{error:?}");
    assert_eq!(entries(&folder), [MIX_FILE, MANIFEST_FILE]);
    assert_eq!(entries(&folder.join(MIX_FILE)), ["keep"]);
    assert_eq!(
        fs::read(folder.join(MANIFEST_FILE)).unwrap(),
        manifest_before
    );
}

#[test]
fn a_missing_song_folder_is_reported() {
    let (dir, _folder, song) = song_folder();
    let missing = dir.path().join("Missing.song");
    let read = read_mix(&missing, &song);
    assert_eq!(read.mix, MixFile::new());
    assert!(
        matches!(read.error, Some(SettingsError::Folder { .. })),
        "{:?}",
        read.error
    );
    let error = write_mix(&missing, &a_mix()).expect_err("no folder");
    assert!(matches!(error, SettingsError::Folder { .. }), "{error:?}");
    assert!(!missing.exists(), "a song folder is never created");
}

#[test]
fn a_mix_too_large_to_check_is_not_written_over() {
    let (_dir, folder, _song) = song_folder();
    let large = format!(
        r#"{{ "formatVersion": 2, "pad": "{}" }}"#,
        "x".repeat(MAX_SETTINGS_FILE_BYTES as usize)
    );
    fs::write(folder.join(MIX_FILE), &large).unwrap();
    let error = write_mix(&folder, &a_mix()).expect_err("the version cannot be checked");
    match &error {
        SettingsError::UncheckedVersion { path, source } => {
            assert!(path.ends_with(MIX_FILE));
            assert!(
                matches!(**source, SettingsError::TooLarge { .. }),
                "{source:?}"
            );
        }
        other => panic!("expected an unchecked version, got {other:?}"),
    }
    assert!(
        error.to_string().contains("was not written over"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(folder.join(MIX_FILE)).unwrap(), large);
    assert_eq!(entries(&folder), [MIX_FILE, MANIFEST_FILE]);
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn mkfifo(path: &Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo {}", path.display());
    }

    #[test]
    fn a_newer_mix_the_check_cannot_read_is_not_written_over() {
        let (_dir, folder, _song) = song_folder();
        fs::write(folder.join(MIX_FILE), r#"{"formatVersion":2}"#).unwrap();
        fs::set_permissions(folder.join(MIX_FILE), fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(folder.join(MIX_FILE)).is_ok() {
            fs::set_permissions(folder.join(MIX_FILE), fs::Permissions::from_mode(0o644)).unwrap();
            eprintln!(
                "note: mode 0o000 does not block reads here (root or CAP_DAC_OVERRIDE); skipping"
            );
            return;
        }
        let result = write_mix(&folder, &a_mix());
        fs::set_permissions(folder.join(MIX_FILE), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(folder.join(MIX_FILE)).unwrap(),
            r#"{"formatVersion":2}"#
        );
        assert!(
            matches!(
                result,
                Err(SettingsError::UncheckedVersion { ref source, .. })
                    if matches!(**source, SettingsError::Read { .. })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_fifo_mix_is_refused_without_blocking() {
        let (_dir, folder, song) = song_folder();
        mkfifo(&folder.join(MIX_FILE));
        let (sender, receiver) = mpsc::channel();
        let reader_folder = folder.clone();
        std::thread::spawn(move || {
            let _ = sender.send(read_mix(&reader_folder, &song));
        });
        let read = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("read_mix returns without opening the FIFO");
        assert_eq!(read.mix, MixFile::new());
        assert!(
            matches!(read.error, Some(SettingsError::NotAFile { .. })),
            "{:?}",
            read.error
        );
    }

    #[test]
    fn a_mix_linked_from_outside_the_song_folder_is_refused_and_left() {
        let (dir, folder, song) = song_folder();
        let outside = dir.path().join("outside.json");
        let contents = a_mix().to_json().unwrap();
        fs::write(&outside, &contents).unwrap();
        symlink(&outside, folder.join(MIX_FILE)).unwrap();

        let read = read_mix(&folder, &song);
        assert_eq!(read.mix, MixFile::new());
        assert!(
            matches!(read.error, Some(SettingsError::Outside { .. })),
            "{:?}",
            read.error
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), contents);

        // A write replaces the link itself, never the file outside.
        write_mix(&folder, &MixFile::new()).expect("written");
        assert_eq!(fs::read_to_string(&outside).unwrap(), contents);
        assert!(
            !fs::symlink_metadata(folder.join(MIX_FILE))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(read_mix(&folder, &song).mix, MixFile::new());
    }

    #[test]
    fn a_mix_linked_inside_the_song_folder_is_read() {
        let (_dir, folder, song) = song_folder();
        fs::write(folder.join("saved-mix.json"), a_mix().to_json().unwrap()).unwrap();
        symlink("saved-mix.json", folder.join(MIX_FILE)).unwrap();
        let read = read_mix(&folder, &song);
        assert!(read.error.is_none(), "{:?}", read.error);
        assert_eq!(read.mix, a_mix());
    }
}
