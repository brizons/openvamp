//! The song facts on Perform (key, tempo, meter, bars) are read from each
//! song folder's `song.json` off the screen's drawing
//! (`openvamp_desktop::facts`): a read that fails is not kept for good, a
//! read that succeeds replaces it, and while it fails the notes say why.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use openvamp_desktop::facts::{FactsCache, RETRY_AFTER, read, unread_note};
use openvamp_desktop::view::SongFacts;

/// A `song.json` in G at 120 BPM in 4/4.
const MANIFEST: &str = r#"{
    "formatVersion": 1,
    "title": "First Light",
    "sampleRate": 48000,
    "key": "G",
    "tempoMap": [{"at": 0, "bpm": 120}],
    "timeSignatureMap": [{"at": 0, "numerator": 4, "denominator": 4}],
    "stems": [],
    "sections": [],
    "arrangements": [{"id": "default", "name": "Default", "sections": []}]
}"#;

/// Reads `folder` as the screen does once the cache asks for it.
fn read_into(cache: &mut FactsCache, folder: &Path, now: Instant) {
    cache.finish(folder.to_owned(), read(folder), now);
}

#[test]
fn a_read_that_fails_is_tried_again_and_one_that_succeeds_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path();
    let mut cache = FactsCache::new();
    let start = Instant::now();

    // Nothing read yet: it is read once, and shows nothing until it is.
    assert!(cache.start_read(folder, start));
    assert!(
        !cache.start_read(folder, start),
        "a read under way is not started again"
    );
    assert_eq!(cache.facts(folder), SongFacts::default());

    // No song.json: the read fails, and says why.
    read_into(&mut cache, folder, start);
    let failed = cache.facts(folder);
    assert_eq!(
        (failed.key.as_deref(), failed.bpm, failed.meter),
        (None, None, None)
    );
    let note = failed.unread.expect("the notes say why");
    assert!(note.contains("song.json"), "{note}");

    // The failure is not kept for good: the volume gets RETRY_AFTER, then
    // the folder is read again, and the note stays while it is.
    let soon = start + RETRY_AFTER - Duration::from_millis(1);
    assert!(!cache.start_read(folder, soon));
    fs::write(folder.join("song.json"), MANIFEST).unwrap();
    let later = start + RETRY_AFTER;
    assert!(cache.start_read(folder, later));
    assert_eq!(cache.facts(folder).unread, Some(note));

    // It reads: the facts replace the failure, and the note goes.
    read_into(&mut cache, folder, later);
    let facts = cache.facts(folder);
    assert_eq!(facts.key.as_deref(), Some("G"));
    assert_eq!(facts.bpm, Some(120.0));
    assert_eq!(facts.meter, Some((4, 4)));
    assert_eq!(facts.unread, None);
    // What was read is kept: not read again while the song shows.
    assert!(!cache.start_read(folder, later + 10 * RETRY_AFTER));
}

#[test]
fn the_note_says_the_header_misses_its_facts_and_why() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path();
    fs::write(folder.join("song.json"), "{").unwrap();
    let reason = read(folder).expect_err("a song.json that does not parse");
    assert!(reason.starts_with("song.json: "), "{reason}");
    assert_eq!(
        unread_note(&reason),
        format!("Key, tempo and meter missing (bars may show as seconds): {reason}. Trying again.")
    );

    let mut cache = FactsCache::new();
    let now = Instant::now();
    assert!(cache.start_read(folder, now));
    read_into(&mut cache, folder, now);
    assert_eq!(cache.facts(folder).unread, Some(unread_note(&reason)));

    // A missing file names the file too.
    let empty = tempfile::tempdir().unwrap();
    let reason = read(empty.path()).expect_err("no song.json");
    assert!(reason.starts_with("song.json: "), "{reason}");
}

#[test]
fn each_song_folder_keeps_its_own_facts() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    fs::write(first.path().join("song.json"), MANIFEST).unwrap();
    let mut cache = FactsCache::new();
    let now = Instant::now();
    assert!(cache.start_read(first.path(), now));
    read_into(&mut cache, first.path(), now);
    // A setlist moves to the next item, whose folder has no song.json.
    assert!(cache.start_read(second.path(), now));
    read_into(&mut cache, second.path(), now);
    assert!(cache.facts(second.path()).unread.is_some());
    // And back: the first item's facts are still there, without a read.
    assert!(!cache.start_read(first.path(), now));
    assert_eq!(cache.facts(first.path()).key.as_deref(), Some("G"));
    assert_eq!(cache.facts(first.path()).unread, None);
}

#[test]
fn a_song_or_setlist_opening_reads_every_folder_again() {
    let read_one = tempfile::tempdir().unwrap();
    let failed = tempfile::tempdir().unwrap();
    let reading = tempfile::tempdir().unwrap();
    fs::write(read_one.path().join("song.json"), MANIFEST).unwrap();
    fs::write(reading.path().join("song.json"), MANIFEST).unwrap();
    let mut cache = FactsCache::new();
    let now = Instant::now();
    for folder in [read_one.path(), failed.path(), reading.path()] {
        assert!(cache.start_read(folder, now));
    }
    read_into(&mut cache, read_one.path(), now);
    read_into(&mut cache, failed.path(), now);

    cache.forget();
    // Read or failed, each is read again at once, and shows what it had
    // until that read comes back.
    assert!(cache.start_read(read_one.path(), now));
    assert!(cache.start_read(failed.path(), now));
    assert_eq!(cache.facts(read_one.path()).key.as_deref(), Some("G"));
    assert!(cache.facts(failed.path()).unread.is_some());
    assert!(!cache.start_read(read_one.path(), now));
    // One whose read is under way is not read twice, and its read counts.
    assert!(!cache.start_read(reading.path(), now));
    read_into(&mut cache, reading.path(), now);
    assert_eq!(cache.facts(reading.path()).key.as_deref(), Some("G"));
}
