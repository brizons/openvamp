//! S4: an edited song is saved by rewriting its `song.json` atomically,
//! after the checks loading makes. An invalid package writes nothing,
//! fields this version does not know stay, and the stems and the
//! `waveforms/` cache are never touched.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use common::{manifest, write_int, write_manifest};
use domain::{
    Arrangement, MixFile, SETLIST_FORMAT_VERSION, SectionMarker, SectionType, Setlist, SongPackage,
    StemRole, ValidationError,
};
use library::{
    Library, LoadError, MANIFEST_FILE, SaveSongError, SettingsError, SongProblem, build_overview,
    load_song, read_mix, write_mix,
};
use serde_json::{Value, json};

const RATE: u32 = 1_000;
const SONG: &str = "Hymns/Song.song";

/// A library with the four-second song [`SONG`]: a drum track, a click,
/// two sections and two arrangements.
fn library_with_song() -> (tempfile::TempDir, Library, PathBuf, SongPackage) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(SONG);
    write_song(&folder);
    let package =
        SongPackage::from_json(&fs::read_to_string(folder.join(MANIFEST_FILE)).unwrap()).unwrap();
    let library = Library::new(dir.path());
    (dir, library, folder, package)
}

fn write_song(folder: &Path) {
    write_int(
        &folder.join("stems/drums.wav"),
        16,
        1,
        RATE,
        &[1_000; 4_000],
    );
    write_int(&folder.join("stems/click.wav"), 16, 1, RATE, &[0; 4_000]);
    let mut package = manifest(
        RATE,
        &[
            ("drums", "stems/drums.wav", StemRole::Track),
            ("click", "stems/click.wav", StemRole::Click),
        ],
    );
    package.artist = Some("Old Artist".to_owned());
    package.sections.push(SectionMarker {
        id: "chorus".to_owned(),
        section_type: SectionType::Chorus,
        number: None,
        start: 2_000,
    });
    package.arrangements[0].sections.push("chorus".to_owned());
    package.arrangements.push(Arrangement {
        id: "short".to_owned(),
        name: "Short".to_owned(),
        sections: vec!["chorus".to_owned()],
        infinite_loop: Vec::new(),
    });
    write_manifest(folder, &package);
}

/// `package` with a new title, key and tempo, a verse between the intro and
/// the chorus, a new arrangement, the chorus looping in Short, and the drums
/// renamed.
fn edited(package: &SongPackage) -> SongPackage {
    let mut edited = package.clone();
    edited.title = "Edited".to_owned();
    edited.key = Some("D".to_owned());
    edited.tempo_map[0].bpm = 96.0;
    edited.sections.insert(
        1,
        SectionMarker {
            id: "verse".to_owned(),
            section_type: SectionType::Verse,
            number: Some(1),
            start: 1_000,
        },
    );
    edited.arrangements[0].sections = vec![
        "intro".to_owned(),
        "verse".to_owned(),
        "chorus".to_owned(),
        "chorus".to_owned(),
    ];
    edited.arrangements[1].infinite_loop = vec!["chorus".to_owned()];
    edited.arrangements.push(Arrangement {
        id: "verse-only".to_owned(),
        name: "Verse only".to_owned(),
        sections: vec!["verse".to_owned()],
        infinite_loop: Vec::new(),
    });
    edited.stems[0].part = "kit".to_owned();
    edited
}

/// Every file under `folder` but `song.json`, with its bytes and modified
/// time, by path relative to `folder`.
fn snapshot(folder: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else if path != root.join(MANIFEST_FILE) {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (fs::read(&path).unwrap(), modified),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(folder, folder, &mut files);
    files
}

/// File names in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn an_edited_song_is_saved_and_loads_as_edited() {
    let (_dir, library, folder, package) = library_with_song();
    let edited = edited(&package);

    let entry = library.save_song(SONG, &edited).unwrap();

    let written = fs::read_to_string(folder.join(MANIFEST_FILE)).unwrap();
    assert_eq!(
        written,
        edited.to_json().unwrap(),
        "written as an import writes it"
    );
    assert_eq!(SongPackage::from_json(&written).unwrap(), edited);
    let loaded = load_song(&folder).unwrap();
    assert_eq!(loaded.song.title(), "Edited");
    assert_eq!(loaded.song.key(), Some("D"));
    assert_eq!(loaded.song.tempo_map()[0].bpm, 96.0);
    let sections: Vec<&str> = loaded
        .song
        .sections()
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(sections, ["intro", "verse", "chorus"]);
    assert_eq!(loaded.song.stems()[0].part, "kit");
    assert_eq!(entry.song, SONG);
    assert_eq!(entry.title, "Edited");
    let names: Vec<&str> = entry.arrangements.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["Default", "Short", "Verse only"]);
    assert_eq!(library.list_songs().unwrap().songs, [entry]);
    assert_eq!(
        entries(&folder),
        [MANIFEST_FILE, "stems"],
        "no temporary file"
    );
}

#[test]
fn an_invalid_package_is_refused_with_every_problem_and_nothing_is_written() {
    let (_dir, library, folder, package) = library_with_song();
    let before = fs::read(folder.join(MANIFEST_FILE)).unwrap();
    let mut broken = edited(&package);
    broken.sections[0].start = 10;
    broken.arrangements[2].sections.push("ghost".to_owned());
    broken.stems[0].file = "stems/missing.wav".to_owned();

    let error = library.save_song(SONG, &broken).unwrap_err();

    let SaveSongError::Invalid(problems) = &error else {
        panic!("{error:?}");
    };
    assert!(
        problems.iter().any(
            |problem| matches!(problem, SongProblem::StemUnreadable { id, .. } if id == "drums")
        ),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|problem| matches!(
            problem,
            SongProblem::Validation(ValidationError::FirstSectionNotAtZero { start: 10, .. })
        )),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|problem| matches!(
            problem,
            SongProblem::Validation(ValidationError::UnknownSectionReference { section, .. })
                if section == "ghost"
        )),
        "{problems:?}"
    );
    assert!(
        error.to_string().contains("song.json was not written"),
        "{error}"
    );
    assert_eq!(fs::read(folder.join(MANIFEST_FILE)).unwrap(), before);
    assert_eq!(entries(&folder), [MANIFEST_FILE, "stems"]);
}

#[test]
fn the_save_checks_the_package_as_loading_does() {
    let (dir, library, _folder, package) = library_with_song();
    let mut broken = edited(&package);
    broken.sample_rate = 2_000;
    broken.arrangements[0].sections.clear();
    broken.stems[1].file = "../click.wav".to_owned();
    let copy = dir.path().join("Copy.song");
    write_song(&copy);
    write_manifest(&copy, &broken);

    let saved = library.save_song(SONG, &broken).unwrap_err();
    let loaded = load_song(&copy).unwrap_err();

    let (SaveSongError::Invalid(saved), LoadError::Invalid(loaded)) = (&saved, &loaded) else {
        panic!("{saved:?} / {loaded:?}");
    };
    assert_eq!(saved, loaded);
    assert!(saved.len() >= 3, "{saved:?}");
}

#[test]
fn fields_this_version_does_not_know_stay_at_every_level() {
    let (_dir, library, folder, package) = library_with_song();
    let mut stored: Value = serde_json::to_value(&package).unwrap();
    stored["x-other-tool"] = json!({ "version": 3, "tags": ["live"] });
    stored["stems"][0]["color"] = json!("red");
    stored["stems"][1]["muted"] = json!(true);
    stored["sections"][0]["color"] = json!("blue");
    stored["sections"][1]["color"] = json!("green");
    stored["arrangements"][0]["notes"] = json!("keep me");
    stored["tempoMap"][0]["label"] = json!("start");
    stored["timeSignatureMap"][0]["accent"] = json!(1);
    fs::write(
        folder.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&stored).unwrap(),
    )
    .unwrap();
    let mut edited = edited(&package);
    edited.artist = None;
    // The chorus goes, and its unknown field with it.
    edited.sections.retain(|section| section.id != "chorus");
    edited.arrangements[0].sections = vec!["intro".to_owned(), "verse".to_owned()];
    edited.arrangements[1].sections = vec!["verse".to_owned()];
    edited.arrangements[1].infinite_loop.clear();

    library.save_song(SONG, &edited).unwrap();

    let text = fs::read_to_string(folder.join(MANIFEST_FILE)).unwrap();
    let written: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        written["x-other-tool"],
        json!({ "version": 3, "tags": ["live"] })
    );
    assert_eq!(written["stems"][0]["color"], "red");
    assert_eq!(written["stems"][0]["part"], "kit");
    assert_eq!(written["stems"][1]["muted"], true);
    assert_eq!(written["sections"][0]["color"], "blue");
    assert_eq!(written["sections"][1]["id"], "verse");
    assert_eq!(written["sections"][1].get("color"), None);
    assert!(!text.contains("green"), "{text}");
    assert_eq!(written["arrangements"][0]["notes"], "keep me");
    assert_eq!(written["tempoMap"][0]["label"], "start");
    assert_eq!(written["tempoMap"][0]["bpm"], 96.0);
    assert_eq!(written["timeSignatureMap"][0]["accent"], 1);
    assert_eq!(written["title"], "Edited");
    assert_eq!(
        written.get("artist"),
        None,
        "a field cleared by the edit goes"
    );
    assert_eq!(SongPackage::from_json(&text).unwrap(), edited);
    assert_eq!(load_song(&folder).unwrap().song.title(), "Edited");
}

#[test]
fn the_stems_and_the_waveforms_cache_are_untouched() {
    let (_dir, library, folder, package) = library_with_song();
    build_overview(&folder, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let before = snapshot(&folder);
    assert!(before.contains_key(Path::new("waveforms/overview.bin")));

    library.save_song(SONG, &edited(&package)).unwrap();

    assert_eq!(snapshot(&folder), before);
    assert_eq!(entries(&folder), [MANIFEST_FILE, "stems", "waveforms"]);
}

#[test]
fn a_missing_song_gives_a_clear_error_and_creates_nothing() {
    let (dir, library, _folder, package) = library_with_song();

    let gone = library.save_song("Hymns/Gone.song", &package).unwrap_err();
    assert!(
        matches!(&gone, SaveSongError::SongFolder { source, .. } if source.kind() == io::ErrorKind::NotFound),
        "{gone:?}"
    );
    assert!(gone.to_string().contains("Gone.song"), "{gone}");
    assert!(!dir.path().join("Hymns/Gone.song").exists());

    fs::create_dir(dir.path().join("Empty.song")).unwrap();
    let empty = library.save_song("Empty.song", &package).unwrap_err();
    assert!(
        matches!(&empty, SaveSongError::ReadManifest { path, source }
            if path.ends_with("Empty.song/song.json") && source.kind() == io::ErrorKind::NotFound),
        "{empty:?}"
    );
    assert!(empty.to_string().contains("song.json"), "{empty}");
    assert_eq!(
        entries(&dir.path().join("Empty.song")),
        Vec::<String>::new()
    );

    let outside = library.save_song("../Song.song", &package).unwrap_err();
    assert!(
        matches!(&outside, SaveSongError::InvalidPath { song } if song == "../Song.song"),
        "{outside:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_song_folder_linked_out_of_the_library_is_refused() {
    let (dir, _library, folder, package) = library_with_song();
    let library_root = dir.path().join("Library");
    fs::create_dir(&library_root).unwrap();
    std::os::unix::fs::symlink(&folder, library_root.join("Linked.song")).unwrap();
    let before = fs::read(folder.join(MANIFEST_FILE)).unwrap();

    let error = Library::new(&library_root)
        .save_song("Linked.song", &edited(&package))
        .unwrap_err();

    assert!(
        matches!(&error, SaveSongError::OutsideLibrary { song } if song == "Linked.song"),
        "{error:?}"
    );
    assert_eq!(fs::read(folder.join(MANIFEST_FILE)).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn a_read_only_song_gives_a_clear_error_and_stays_as_it_was() {
    use std::os::unix::fs::PermissionsExt;

    let mode = |path: &Path, mode| fs::set_permissions(path, fs::Permissions::from_mode(mode));
    let (_dir, library, folder, package) = library_with_song();
    let manifest_path = folder.join(MANIFEST_FILE);
    let before = fs::read(&manifest_path).unwrap();

    mode(&manifest_path, 0o444).unwrap();
    let file = library.save_song(SONG, &edited(&package));
    mode(&manifest_path, 0o644).unwrap();
    let error = file.unwrap_err();
    assert!(
        matches!(&error, SaveSongError::Write { path, source }
            if path.ends_with("Hymns/Song.song/song.json") && source.kind() == io::ErrorKind::PermissionDenied),
        "{error:?}"
    );
    assert!(error.to_string().contains("read-only"), "{error}");
    assert_eq!(fs::read(&manifest_path).unwrap(), before);

    mode(&folder, 0o555).unwrap();
    if fs::write(folder.join("probe"), b"").is_ok() {
        // Running as root: the folder stays writable.
        mode(&folder, 0o755).unwrap();
        return;
    }
    let read_only_folder = library.save_song(SONG, &edited(&package));
    mode(&folder, 0o755).unwrap();
    let error = read_only_folder.unwrap_err();
    assert!(
        matches!(&error, SaveSongError::Write { path, source }
            if path.ends_with("Hymns/Song.song/song.json") && source.kind() == io::ErrorKind::PermissionDenied),
        "{error:?}"
    );
    assert!(error.to_string().starts_with("cannot write"), "{error}");
    assert_eq!(fs::read(&manifest_path).unwrap(), before);
    assert_eq!(entries(&folder), [MANIFEST_FILE, "stems"]);
}

#[test]
fn a_song_json_this_version_cannot_read_is_not_written_over() {
    let (_dir, library, folder, package) = library_with_song();
    let manifest_path = folder.join(MANIFEST_FILE);
    for stored in [
        r#"{"formatVersion":2,"title":"From the future"}"#,
        "{ not json",
    ] {
        fs::write(&manifest_path, stored).unwrap();

        let error = library.save_song(SONG, &edited(&package)).unwrap_err();

        assert!(
            matches!(&error, SaveSongError::Manifest { path, .. } if path.ends_with("Hymns/Song.song/song.json")),
            "{error:?}"
        );
        assert!(
            error.to_string().contains("was not written over"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&manifest_path).unwrap(), stored);
    }
}

/// The save shares the atomic write and the confined read with setlists and
/// settings files, which still behave as before.
#[test]
fn setlists_and_mixes_are_still_written_whole_and_read_as_text() {
    let (dir, library, folder, _package) = library_with_song();
    let setlist = Setlist {
        format_version: SETLIST_FORMAT_VERSION,
        name: "Sunday".to_owned(),
        items: Vec::new(),
    };

    let entry = library.save_setlist(&setlist).unwrap();
    assert_eq!(library.load_setlist(&entry.file).unwrap(), setlist);
    let setlists = dir.path().join(library::SETLISTS_FOLDER);
    assert_eq!(
        fs::read_to_string(setlists.join(&entry.file)).unwrap(),
        setlist.to_json().unwrap()
    );
    assert_eq!(entries(&setlists), [entry.file.as_str()]);

    let song = load_song(&folder).unwrap().song;
    write_mix(&folder, &MixFile::new()).unwrap();
    assert_eq!(
        fs::read_to_string(folder.join(library::MIX_FILE)).unwrap(),
        MixFile::new().to_json().unwrap()
    );
    let read = read_mix(&folder, &song);
    assert_eq!((read.mix, read.error.is_none()), (MixFile::new(), true));

    fs::write(folder.join(library::MIX_FILE), [0xFF, 0xFE]).unwrap();
    assert!(matches!(
        read_mix(&folder, &song).error,
        Some(SettingsError::NotText { .. })
    ));
}
