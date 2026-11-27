//! The Song editor's jobs off the UI thread: reading a library song
//! ([`OpenSongJob`]), reading or building its waveform ([`WaveformJob`]),
//! and saving it ([`SaveSongJob`]), with the setlists that play part of it
//! ([`SetlistUses`], [`SetlistFix`]). Each runs on the screen's background
//! executor and hands back a result with its ticket, so a result of a job
//! a newer one replaced, or of an editor since closed, changes nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use domain::{SetlistItem, SongPackage, StemInfo, ValidationError, validate};
use library::{
    Library, MANIFEST_FILE, Overview, OverviewError, OverviewProgress, SaveSongError, SetlistError,
    SongEntry, SongListing, build_overview, load_overview, read_mix, write_mix,
};

use crate::editor::{Edit, SetlistEditor};
use crate::song_import::{Finish, Run, next_ticket, sentence};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the editor needs of a song, read by an [`OpenSongJob`].
#[derive(Debug)]
pub(crate) struct SongSource {
    pub(super) package: SongPackage,
    pub(super) length: u64,
    /// What the library's setlists play of the song.
    pub(super) uses: SetlistUses,
}

/// What the setlists of a library play of one song, read when the editor
/// opens: which arrangement each item plays, and from where to where.
#[derive(Debug, Clone, Default)]
pub(crate) struct SetlistUses {
    /// Every setlist with an item of the song.
    pub(super) setlists: Vec<SetlistUse>,
    /// Why some setlists could not be read, so what they play is not known:
    /// a file of the setlists folder, named, or the folder itself. Empty
    /// when every setlist was read.
    pub(super) unread: Vec<String>,
}

/// A setlist with items of the song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetlistUse {
    /// Its file in the setlists folder.
    pub(super) file: String,
    /// Its name.
    pub(super) name: String,
    /// Its items of the song, in setlist order.
    pub(super) items: Vec<ItemUse>,
}

/// What one item of a setlist plays of the song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemUse {
    /// The arrangement's id.
    pub(super) arrangement: String,
    /// Its first position, when it does not start the arrangement.
    pub(super) start: Option<usize>,
    /// Its last position, when it does not end it.
    pub(super) end: Option<usize>,
}

impl SetlistUses {
    /// Reads every setlist of `library` for its items of `song`. A setlist
    /// that cannot be read, or a setlists folder that cannot be listed, is
    /// kept as unread, so whatever depends on what the setlists play can
    /// refuse rather than guess.
    pub(super) fn read(library: &Library, song: &str) -> Self {
        let mut uses = Self::default();
        let listing = match library.list_setlists() {
            Ok(listing) => listing,
            Err(error) => {
                uses.unread.push(error.to_string());
                return uses;
            }
        };
        uses.unread
            .extend(listing.unreadable.iter().map(|file| file.file.clone()));
        uses.unread
            .extend(listing.entry_errors.iter().map(ToString::to_string));
        for entry in &listing.setlists {
            let setlist = match library.load_setlist(&entry.file) {
                Ok(setlist) => setlist,
                Err(_) => {
                    uses.unread.push(entry.file.clone());
                    continue;
                }
            };
            let items: Vec<ItemUse> = setlist
                .items
                .iter()
                .filter_map(|item| match item {
                    SetlistItem::Song(item) if item.song == song => Some(ItemUse {
                        arrangement: item.arrangement.clone(),
                        start: item.start_position,
                        end: item.end_position,
                    }),
                    _ => None,
                })
                .collect();
            if !items.is_empty() {
                uses.setlists.push(SetlistUse {
                    file: entry.file.clone(),
                    name: setlist.name.clone(),
                    items,
                });
            }
        }
        uses
    }

    /// The names of the setlists that play arrangement `id`, once each.
    pub(super) fn playing(&self, id: &str) -> Vec<String> {
        self.setlists
            .iter()
            .filter(|setlist| setlist.items.iter().any(|item| item.arrangement == id))
            .map(|setlist| setlist.name.clone())
            .collect()
    }
}

/// What a save changes in a setlist item that plays part of an edited
/// arrangement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetlistFix {
    /// The setlist's file and name.
    pub(super) file: String,
    pub(super) name: String,
    /// The arrangement's id and name.
    pub(super) arrangement: String,
    pub(super) arrangement_name: String,
    /// The item's start and end as the setlist has them.
    pub(super) from: (Option<usize>, Option<usize>),
    /// What they become: the positions that play the same audio, or the
    /// whole arrangement when `lost`.
    pub(super) to: (Option<usize>, Option<usize>),
    /// Whether positions the item played were removed or play in another
    /// order, so it plays the whole arrangement.
    pub(super) lost: bool,
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
        uses: SetlistUses::read(library, song),
    })
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
/// `mix.json`, moves the setlist items that play part of an edited
/// arrangement ([`SetlistFix`]), and lists the library's songs again; run
/// off the UI thread.
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
    /// The setlist items to move, once the song is saved.
    pub(super) fixes: Vec<SetlistFix>,
    pub(super) finish: Finish,
}

impl SaveSongJob {
    /// Saves the song, then its setlists; it blocks until done.
    pub fn run(self) -> SongSaved {
        // Set under the stage's lock: a quit waiting for the save sees it.
        self.finish.0.set(Run::Running);
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
        let (notes, moves) = if result.is_ok() {
            write_fixes(&self.library, &self.song, &self.fixes)
        } else {
            (Vec::new(), Vec::new())
        };
        let listing = result.is_ok().then(|| self.library.list_songs());
        let saved = SongSaved {
            ticket: self.ticket,
            root: self.library.root().to_owned(),
            result,
            mix_problem,
            setlist_notes: notes,
            moves,
            listing,
        };
        drop(self.finish);
        saved
    }
}

/// Moves the setlist items `fixes` names in the setlists of `library`, as
/// the setlist editor edits them ([`Edit::StartEnd`]); every item of
/// `song` with the arrangement and positions a fix was made for moves.
/// Returns what to tell (an item that now plays a whole arrangement, a
/// setlist that could not be written) and the setlist files that moved.
fn write_fixes(
    library: &Library,
    song: &str,
    fixes: &[SetlistFix],
) -> (Vec<String>, Vec<(String, String)>) {
    let mut notes = Vec::new();
    let mut moves = Vec::new();
    // Each file once, in the order the fixes name them: a file opened twice
    // would move its items twice.
    let mut files: Vec<&str> = Vec::new();
    for fix in fixes {
        if !files.contains(&fix.file.as_str()) {
            files.push(fix.file.as_str());
        }
    }
    for file in files {
        let of_file: Vec<&SetlistFix> = fixes.iter().filter(|fix| fix.file == file).collect();
        let name = &of_file[0].name;
        let mut editor = match SetlistEditor::open(library.clone(), file) {
            Ok(editor) => editor,
            Err(error) => {
                notes.push(not_updated(name, &error.to_string()));
                continue;
            }
        };
        // Every item is matched against the setlist as it was read, before
        // any edit, so an item one fix moves never matches a later fix's
        // old positions and moves again.
        let planned: Vec<_> = editor
            .setlist()
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                SetlistItem::Song(item) if item.song == song => of_file
                    .iter()
                    .find(|fix| {
                        item.arrangement == fix.arrangement
                            && (item.start_position, item.end_position) == fix.from
                    })
                    .map(|fix| (index, fix.to)),
                _ => None,
            })
            .collect();
        let mut failed = None;
        for (index, (start, end)) in planned {
            if let Err(error) = editor.apply(Edit::StartEnd(index, start, end)) {
                failed.get_or_insert(error.to_string());
            }
        }
        match failed {
            Some(error) => notes.push(not_updated(name, &error)),
            None => notes.extend(of_file.iter().filter(|fix| fix.lost).map(|fix| {
                format!(
                    "\"{}\" now plays all of {}.",
                    fix.name, fix.arrangement_name
                )
            })),
        }
        if editor.file() != file {
            moves.push((file.to_owned(), editor.file().to_owned()));
        }
    }
    (notes, moves)
}

/// Why setlist `name` still plays what it did before the edit.
fn not_updated(name: &str, error: &str) -> String {
    sentence(&format!(
        "\"{name}\" could not be updated to the edit: {error}"
    ))
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
    /// What happened to the setlists the save moved: an item now playing a
    /// whole arrangement, a setlist that could not be written.
    pub(crate) setlist_notes: Vec<String>,
    /// The setlist files that moved, as (from, to): a file named by hand
    /// moves to its name's on its first change.
    pub(crate) moves: Vec<(String, String)>,
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
