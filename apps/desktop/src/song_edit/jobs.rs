//! The Song editor's jobs off the UI thread: reading a library song
//! ([`OpenSongJob`]), reading or building its waveform ([`WaveformJob`]),
//! and saving it ([`SaveSongJob`]). Each runs on the screen's background
//! executor and hands back a result with its ticket, so a result of a job
//! a newer one replaced, or of an editor since closed, changes nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use domain::{SetlistItem, SongPackage, StemInfo, ValidationError, validate};
use library::{
    Library, MANIFEST_FILE, Overview, OverviewError, OverviewProgress, SaveSongError, SetlistError,
    SongEntry, SongListing, build_overview, load_overview, read_mix, write_mix,
};

use crate::song_import::{next_ticket, sentence};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the editor needs of a song, read by an [`OpenSongJob`].
#[derive(Debug)]
pub(crate) struct SongSource {
    pub(super) package: SongPackage,
    pub(super) length: u64,
    /// The names of the setlists that play each arrangement, by its id.
    pub(super) in_use: BTreeMap<String, Vec<String>>,
}

/// Reads the song at `song` (a path in the library, as listed) for the
/// Song editor; run off the UI thread, since it reads the song's files and
/// every setlist of the library.
#[derive(Debug)]
pub struct OpenSongJob {
    ticket: u64,
    library: Library,
    song: String,
}

impl OpenSongJob {
    pub(crate) fn new(library: &Library, song: &str) -> Self {
        Self {
            ticket: next_ticket(),
            library: library.clone(),
            song: song.to_owned(),
        }
    }

    pub(crate) fn ticket(&self) -> u64 {
        self.ticket
    }

    /// Reads the song's `song.json`, its length (the library's listing of
    /// it) and the setlists that use it; it blocks until done.
    pub fn run(self) -> OpenedSong {
        let listing = self.library.list_songs();
        let result = read_source(&self.library, &self.song, &listing);
        OpenedSong {
            ticket: self.ticket,
            song: self.song,
            result,
            listing: Some(listing),
        }
    }
}

/// What an [`OpenSongJob`] read, for
/// [`crate::library_page::LibraryPage::song_opened`].
#[derive(Debug)]
pub struct OpenedSong {
    pub(crate) ticket: u64,
    pub(crate) song: String,
    pub(crate) result: Result<SongSource, String>,
    /// The library's songs as they are now.
    pub(crate) listing: Option<Result<SongListing, SetlistError>>,
}

fn read_source(
    library: &Library,
    song: &str,
    listing: &Result<SongListing, SetlistError>,
) -> Result<SongSource, String> {
    let entry = match listing {
        Ok(listing) => listing.songs.iter().find(|entry| entry.song == song),
        Err(error) => return Err(error.to_string()),
    };
    let entry = entry.ok_or_else(|| match listing {
        Ok(listing) => listing
            .unreadable
            .iter()
            .find(|unreadable| unreadable.song == song)
            .map_or_else(
                || "it is no longer in the library".to_owned(),
                |unreadable| unreadable.error.to_string(),
            ),
        Err(error) => error.to_string(),
    })?;
    let path = library.root().join(song).join(MANIFEST_FILE);
    let json = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let package = SongPackage::from_json(&json).map_err(|error| error.to_string())?;
    Ok(SongSource {
        package,
        length: entry.length_frames,
        in_use: setlists_using(library, song),
    })
}

/// The names of the setlists of `library` that play `song`, by the id of
/// the arrangement they play. A setlist that cannot be read is left out:
/// the Library lists it as unreadable.
fn setlists_using(library: &Library, song: &str) -> BTreeMap<String, Vec<String>> {
    let mut used: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(listing) = library.list_setlists() else {
        return used;
    };
    for entry in &listing.setlists {
        let Ok(setlist) = library.load_setlist(&entry.file) else {
            continue;
        };
        for item in &setlist.items {
            if let SetlistItem::Song(item) = item
                && item.song == song
            {
                let names = used.entry(item.arrangement.clone()).or_default();
                if !names.contains(&setlist.name) {
                    names.push(setlist.name.clone());
                }
            }
        }
    }
    used
}

/// How far a waveform build has got, written by its thread.
type WaveProgress = Arc<Mutex<Option<OverviewProgress>>>;

/// Reads a song's waveform overview: the cached one ([`load_overview`]),
/// else one built from its stems ([`build_overview`]), with its progress;
/// run off the UI thread. Closing the editor cancels it.
#[derive(Debug)]
pub struct WaveformJob {
    ticket: u64,
    folder: PathBuf,
    progress: WaveProgress,
    cancel: Arc<AtomicBool>,
}

impl WaveformJob {
    /// Reads or builds the overview; it blocks until done, cancelled, or
    /// failed. A job cancelled before it starts reads nothing.
    pub fn run(self) -> Waved {
        let result = if self.cancel.load(Ordering::SeqCst) {
            Err(OverviewError::Cancelled)
        } else if let Some(overview) = load_overview(&self.folder) {
            Ok(overview)
        } else {
            let progress = &self.progress;
            build_overview(
                &self.folder,
                &mut |update| *lock(progress) = Some(update),
                &self.cancel,
            )
        };
        Waved {
            ticket: self.ticket,
            result,
        }
    }
}

/// What a [`WaveformJob`] gave, for
/// [`crate::library_page::LibraryPage::waveform_done`].
#[derive(Debug)]
pub struct Waved {
    pub(crate) ticket: u64,
    pub(crate) result: Result<Overview, OverviewError>,
}

impl Waved {
    /// The overview read, or why there is none.
    pub fn overview(&self) -> Result<&Overview, &OverviewError> {
        self.result.as_ref()
    }
}

/// A waveform job under way; dropping it (the editor closing; on Perform,
/// another song showing or play starting) cancels the job.
#[derive(Debug)]
pub(crate) struct WaveformRun {
    pub(crate) ticket: u64,
    progress: WaveProgress,
    cancel: Arc<AtomicBool>,
    /// The percentage last drawn ([`super::SongEdit::waveform_moved`]).
    pub(super) drawn: Option<u32>,
}

impl WaveformRun {
    /// A waveform job for the song folder `folder`, and the run the editor
    /// (or the Perform song map, [`crate::map_wave`]) keeps of it.
    pub(crate) fn start(folder: PathBuf) -> (WaveformJob, Self) {
        let progress = WaveProgress::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let ticket = next_ticket();
        let job = WaveformJob {
            ticket,
            folder,
            progress: Arc::clone(&progress),
            cancel: Arc::clone(&cancel),
        };
        let run = Self {
            ticket,
            progress,
            cancel,
            drawn: None,
        };
        (job, run)
    }

    pub(super) fn percent(&self) -> Option<u32> {
        lock(&self.progress).map(|progress| (progress.fraction().clamp(0.0, 1.0) * 100.0) as u32)
    }
}

impl Drop for WaveformRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Where the editor's waveform stands.
#[derive(Debug)]
pub(super) enum Waveform {
    Reading(WaveformRun),
    Ready(Overview),
    Failed(String),
}

/// Saves the edited package as the song's `song.json`
/// ([`Library::save_song`]), drops the deleted arrangements from its
/// `mix.json`, and lists the library's songs again; run off the UI thread.
#[derive(Debug)]
pub struct SaveSongJob {
    pub(super) ticket: u64,
    pub(super) library: Library,
    pub(super) song: String,
    pub(super) package: SongPackage,
    pub(super) length: u64,
    /// The ids of the arrangements the edit deleted, which `mix.json` is
    /// to forget.
    pub(super) deleted: Vec<String>,
}

impl SaveSongJob {
    /// Saves the song; it blocks until done.
    pub fn run(self) -> SongSaved {
        let result = self.library.save_song(&self.song, &self.package);
        let mix_problem = match &result {
            Ok(_) if !self.deleted.is_empty() => forget_mixes(
                &self.library.root().join(&self.song),
                &self.package,
                self.length,
                &self.deleted,
            )
            .err(),
            _ => None,
        };
        let listing = result.is_ok().then(|| self.library.list_songs());
        SongSaved {
            ticket: self.ticket,
            root: self.library.root().to_owned(),
            result,
            mix_problem,
            listing,
        }
    }
}

/// What a [`SaveSongJob`] did, for
/// [`crate::library_page::LibraryPage::song_saved`].
#[derive(Debug)]
pub struct SongSaved {
    pub(crate) ticket: u64,
    pub(crate) root: PathBuf,
    pub(crate) result: Result<SongEntry, SaveSongError>,
    /// Why `mix.json` still has a deleted arrangement's mix.
    pub(crate) mix_problem: Option<String>,
    pub(crate) listing: Option<Result<SongListing, SetlistError>>,
}

/// Drops the mixes of the arrangements `deleted` from the `mix.json` of the
/// song folder `dir`, which now holds `package`; arrangements the package
/// still has (an id a new arrangement took again) keep theirs.
fn forget_mixes(
    dir: &Path,
    package: &SongPackage,
    length: u64,
    deleted: &[String],
) -> Result<(), String> {
    let infos: Vec<StemInfo> = package
        .stems
        .iter()
        .map(|stem| StemInfo {
            id: stem.id.clone(),
            sample_rate: package.sample_rate,
            length_samples: length,
        })
        .collect();
    let song = validate(package, &infos)
        .map_err(|errors| package_problems(&errors))?
        .song;
    let mut read = read_mix(dir, &song);
    if let Some(error) = read.error {
        return Err(format!("mix.json was not updated: {error}"));
    }
    let mut forgot = false;
    for id in deleted {
        if !package.arrangements.iter().any(|kept| &kept.id == id) {
            forgot |= read.mix.arrangements.remove(id).is_some();
        }
    }
    if forgot {
        write_mix(dir, &read.mix).map_err(|error| format!("mix.json was not updated: {error}"))?;
    }
    Ok(())
}

/// The package rules `errors` broke, as one sentence each.
pub(super) fn package_problems(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|error| sentence(&error.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}
