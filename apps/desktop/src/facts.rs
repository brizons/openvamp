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
//! ([`FactsCache::forget`]). Meanwhile the notes say why ([`unread_note`]),
//! unless an earlier read gave facts: a re-read that fails keeps them. A
//! read that has not come back after [`READ_TIMEOUT`] counts as failed, so
//! a volume that does not answer is tried again too. A folder is read once
//! at a time until then, and never by more than [`MAX_READS_OUT`] reads, so
//! a volume that never answers holds up that many background threads per
//! folder at most; whichever read comes back last decides what shows.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use library::MANIFEST_FILE;

use crate::view::SongFacts;

/// How long a folder whose read failed waits before it is read again.
pub const RETRY_AFTER: Duration = Duration::from_secs(5);

/// How long a read may take before it counts as failed.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The most reads of one folder under way at once: one, and a retry after
/// it timed out.
pub const MAX_READS_OUT: usize = 2;

/// What the reads of song folders gave, by folder.
#[derive(Debug, Default)]
pub struct FactsCache {
    folders: HashMap<PathBuf, Entry>,
}

/// One song folder's reads.
#[derive(Debug, Default)]
struct Entry {
    /// What the last read that gave facts gave.
    facts: Option<SongFacts>,
    /// Why the last read failed ([`unread_note`]), and when; `None` once
    /// one gives facts.
    failed: Option<(String, Instant)>,
    /// When the read under way started, until it comes back or times out.
    reading: Option<Instant>,
    /// Reads started that have not come back, timed out ones included.
    out: usize,
    /// A song or setlist opened since the last read came back.
    stale: bool,
}

impl FactsCache {
    /// Nothing read yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to start a read of `folder` at `now`: it was never read, a
    /// song or setlist opened since, or its last read failed at least
    /// [`RETRY_AFTER`] ago. Never while a read of it is under way: one that
    /// started [`READ_TIMEOUT`] ago or more fails at `now` instead
    /// ([`timeout_reason`]), and is waited for no longer; and never while
    /// [`MAX_READS_OUT`] reads of it have not come back. When it says so,
    /// the read counts as under way until [`Self::finish`].
    pub fn start_read(&mut self, folder: &Path, now: Instant) -> bool {
        let entry = self.folders.entry(folder.to_owned()).or_default();
        if let Some(started) = entry.reading {
            if now.saturating_duration_since(started) < READ_TIMEOUT {
                return false;
            }
            entry.reading = None;
            entry.fail(&timeout_reason(), now);
        }
        let wanted = entry.out < MAX_READS_OUT
            && (entry.stale
                || match &entry.failed {
                    Some((_, at)) => now.saturating_duration_since(*at) >= RETRY_AFTER,
                    None => entry.facts.is_none(),
                });
        if wanted {
            entry.reading = Some(now);
            entry.out += 1;
        }
        wanted
    }

    /// A read of `folder` came back at `now` with `read`: the facts, which
    /// replace what it had, or why there are none ([`read`]), which keeps
    /// the facts an earlier read gave.
    pub fn finish(&mut self, folder: PathBuf, read: Result<SongFacts, String>, now: Instant) {
        let entry = self.folders.entry(folder).or_default();
        entry.reading = None;
        entry.out = entry.out.saturating_sub(1);
        match read {
            Ok(facts) => {
                entry.stale = false;
                entry.facts = Some(facts);
                entry.failed = None;
            }
            Err(reason) => entry.fail(&reason, now),
        }
    }

    /// What to show for `folder`: the facts its last read that gave any
    /// gave; with none, while its reads fail, no facts and why
    /// ([`SongFacts::unread`]); nothing before a read comes back. A read
    /// under way shows what the last one gave.
    pub fn facts(&self, folder: &Path) -> SongFacts {
        let Some(entry) = self.folders.get(folder) else {
            return SongFacts::default();
        };
        match (&entry.facts, &entry.failed) {
            (Some(facts), _) => facts.clone(),
            (None, Some((note, _))) => SongFacts {
                unread: Some(note.clone()),
                ..SongFacts::default()
            },
            (None, None) => SongFacts::default(),
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

impl Entry {
    /// A read failed at `now` for `reason`: the retry rule applies from
    /// `now`, and the facts an earlier read gave stay.
    fn fail(&mut self, reason: &str, now: Instant) {
        self.stale = false;
        self.failed = Some((unread_note(reason), now));
    }
}

/// Why a read that has not come back after [`READ_TIMEOUT`] failed: "song.json:
/// the drive did not answer within 5 seconds".
pub fn timeout_reason() -> String {
    format!(
        "{MANIFEST_FILE}: the drive did not answer within {} seconds",
        READ_TIMEOUT.as_secs()
    )
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
