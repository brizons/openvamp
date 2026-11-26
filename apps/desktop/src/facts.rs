//! The song facts the Perform screen shows ([`SongFacts`]: key, tempo,
//! meter, bars), read from each song folder's `song.json` off the screen's
//! drawing, so a slow, sleeping or just-unplugged volume never holds up a
//! frame or a control, Panic included. [`FactsCache`] says when a folder is
//! to be read and keeps what each read gave; the screen runs [`read`] on a
//! background thread and hands its result back with [`FactsCache::finish`].
//! Until then the song header shows without them.
//!
//! The retry rule: a read that fails is not kept for good. While its song
//! is on show, the folder is read again [`RETRY_AFTER`] after it failed,
//! and every folder is read again when a song or setlist opens
//! ([`FactsCache::forget`]). Meanwhile the notes say why ([`unread_note`]).
//! A folder is never read twice at once, so a volume that does not answer
//! holds up one background thread per folder at most.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use library::MANIFEST_FILE;

use crate::view::SongFacts;

/// How long a folder whose read failed waits before it is read again.
pub const RETRY_AFTER: Duration = Duration::from_secs(5);

/// What the reads of song folders gave, by folder.
#[derive(Debug, Default)]
pub struct FactsCache {
    folders: HashMap<PathBuf, Entry>,
}

/// One song folder's reads.
#[derive(Debug, Default)]
struct Entry {
    /// What the last read that came back gave; `None` before one does.
    last: Option<Outcome>,
    /// A read is under way.
    reading: bool,
    /// A song or setlist opened since the last read came back.
    stale: bool,
}

/// What a read gave.
#[derive(Debug)]
enum Outcome {
    /// The facts.
    Read(SongFacts),
    /// Why it failed ([`unread_note`]), and when.
    Failed { note: String, at: Instant },
}

impl FactsCache {
    /// Nothing read yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to start a read of `folder` at `now`: it was never read, a
    /// song or setlist opened since, or its last read failed at least
    /// [`RETRY_AFTER`] ago; never while a read of it is under way. When it
    /// says so, the read counts as under way until [`Self::finish`].
    pub fn start_read(&mut self, folder: &Path, now: Instant) -> bool {
        let wanted = self.folders.get(folder).is_none_or(|entry| {
            !entry.reading
                && (entry.stale
                    || match &entry.last {
                        None => true,
                        Some(Outcome::Read(_)) => false,
                        Some(Outcome::Failed { at, .. }) => {
                            now.saturating_duration_since(*at) >= RETRY_AFTER
                        }
                    })
        });
        if wanted {
            self.folders.entry(folder.to_owned()).or_default().reading = true;
        }
        wanted
    }

    /// The read of `folder` came back at `now` with `read`: the facts, or
    /// why there are none ([`read`]). Its facts replace what it had.
    pub fn finish(&mut self, folder: PathBuf, read: Result<SongFacts, String>, now: Instant) {
        let entry = self.folders.entry(folder).or_default();
        entry.reading = false;
        entry.stale = false;
        entry.last = Some(match read {
            Ok(facts) => Outcome::Read(facts),
            Err(reason) => Outcome::Failed {
                note: unread_note(&reason),
                at: now,
            },
        });
    }

    /// What to show for `folder`: what its last read gave, and while it
    /// failed, no facts and why ([`SongFacts::unread`]); nothing before a
    /// read comes back. A read under way shows what the last one gave.
    pub fn facts(&self, folder: &Path) -> SongFacts {
        match self
            .folders
            .get(folder)
            .and_then(|entry| entry.last.as_ref())
        {
            Some(Outcome::Read(facts)) => facts.clone(),
            Some(Outcome::Failed { note, .. }) => SongFacts {
                unread: Some(note.clone()),
                ..SongFacts::default()
            },
            None => SongFacts::default(),
        }
    }

    /// A song or setlist opened: every folder is read again when it shows,
    /// keeping what it shows until then. A read under way is not started
    /// again; its result counts.
    pub fn forget(&mut self) {
        for entry in self.folders.values_mut() {
            entry.stale = true;
        }
    }
}

/// The facts of the song folder `folder`, from its `song.json`, or why
/// there are none, such as "song.json: No such file or directory (os error
/// 2)". It waits for the volume, so it runs off the screen's drawing.
pub fn read(folder: &Path) -> Result<SongFacts, String> {
    let json = fs::read_to_string(folder.join(MANIFEST_FILE))
        .map_err(|error| format!("{MANIFEST_FILE}: {error}"))?;
    SongFacts::parse(&json).map_err(|error| error.to_string())
}

/// The note the notes show while a song's facts cannot be read, for
/// `reason` ([`read`]).
pub fn unread_note(reason: &str) -> String {
    format!("Key, tempo and meter missing (bars may show as seconds): {reason}. Trying again.")
}
