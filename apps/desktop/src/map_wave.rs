//! The waveform on the Perform song map (S5), without a window: the song
//! on show's overview, with the bar lines of its `song.json`, read off the
//! UI thread. [`MapWaveform`] says when the song on show needs one, keeps
//! what it gave, cancels the job of a song no longer on show and drops its
//! late result; [`crate::view::PerformView::with_wave`] draws its bars.
//!
//! OpenVamp plays live, so Perform never reads every stem while a song
//! plays from the same drive: the cached overview ([`load_overview`],
//! which reads no stem) is read whatever the transport does, but one is
//! built from the stems (the Song editor's [`WaveformJob`]) only while the
//! transport is stopped or paused. Play cancels a build under way, which
//! leaves no cache behind, and the next stop or pause starts it again.

use std::fs;
use std::path::{Path, PathBuf};

use app::TransportState;
use domain::{BeatGrid, SongPackage};
use library::{MANIFEST_FILE, Overview, OverviewError, load_overview};

use crate::song_edit::{WaveformJob, WaveformRun};
use crate::song_import::next_ticket;

/// A song's waveform overview and the source frame where each of its bars
/// starts, which the song map's bars are measured by.
#[derive(Debug, Clone, PartialEq)]
pub struct SongWave {
    overview: Overview,
    bar_lines: Vec<u64>,
}

impl SongWave {
    /// The waveform of a song with `overview` whose bars start at
    /// `bar_lines` (sorted source frames, as
    /// [`domain::BeatGrid::bar_lines`] gives them).
    pub fn new(overview: Overview, bar_lines: Vec<u64>) -> Self {
        Self {
            overview,
            bar_lines,
        }
    }

    /// The song's overview.
    pub fn overview(&self) -> &Overview {
        &self.overview
    }

    /// Where each bar starts, in source frames.
    pub fn bar_lines(&self) -> &[u64] {
        &self.bar_lines
    }
}

/// Why a song has no waveform on the song map.
#[derive(Debug)]
pub enum SongWaveError {
    /// The overview could not be read or built, or its build was
    /// cancelled.
    Overview(OverviewError),
    /// The song's `song.json` gives no bar grid: it cannot be read, or its
    /// tempo or meter map breaks a package rule.
    Grid(String),
    /// The song has no cached overview to read; one is built from its
    /// stems once the transport is stopped or paused.
    NotCached,
}

/// Reads the waveform of a song on Perform: its cached overview, or one
/// built from its stems, and its bar lines; run off the UI thread.
#[derive(Debug)]
pub struct SongWaveJob {
    ticket: u64,
    folder: PathBuf,
    step: Step,
}

/// How a [`SongWaveJob`] gets the overview.
#[derive(Debug)]
enum Step {
    /// It reads the cached one; no stem is decoded.
    Cache,
    /// It builds one from the stems, cancelled when its run is dropped.
    Build(WaveformJob),
}

impl SongWaveJob {
    /// Whether it builds the overview from the song's stems, which only a
    /// stopped or paused transport allows; else it reads the cached one.
    pub fn builds(&self) -> bool {
        matches!(self.step, Step::Build(_))
    }

    /// Reads or builds the overview, then the bar lines; it blocks until
    /// done, cancelled, or failed.
    pub fn run(self) -> SongWaveDone {
        let overview = match self.step {
            Step::Cache => load_overview(&self.folder).ok_or(SongWaveError::NotCached),
            Step::Build(job) => job.run().result.map_err(SongWaveError::Overview),
        };
        let result = overview.and_then(|overview| {
            let bar_lines = bar_lines(&self.folder, &overview)?;
            Ok(SongWave::new(overview, bar_lines))
        });
        SongWaveDone {
            ticket: self.ticket,
            result,
        }
    }
}

/// Where each bar of the song in `folder` starts, from the tempo and meter
/// maps of its `song.json`, up to the end of `overview`: the bar grid the
/// session counts its bars with.
fn bar_lines(folder: &Path, overview: &Overview) -> Result<Vec<u64>, SongWaveError> {
    let path = folder.join(MANIFEST_FILE);
    let json = fs::read_to_string(&path)
        .map_err(|error| SongWaveError::Grid(format!("cannot read {}: {error}", path.display())))?;
    let package =
        SongPackage::from_json(&json).map_err(|error| SongWaveError::Grid(error.to_string()))?;
    let grid = BeatGrid::from_maps(
        package.sample_rate,
        overview.length_frames(),
        &package.tempo_map,
        &package.time_signature_map,
    )
    .ok_or_else(|| SongWaveError::Grid("its tempo or meter map gives no bars".to_owned()))?;
    Ok(grid.bar_lines().to_vec())
}

/// What a [`SongWaveJob`] gave, for [`MapWaveform::finish`].
#[derive(Debug)]
pub struct SongWaveDone {
    ticket: u64,
    result: Result<SongWave, SongWaveError>,
}

impl SongWaveDone {
    /// The waveform read, or why there is none.
    pub fn result(&self) -> Result<&SongWave, &SongWaveError> {
        self.result.as_ref()
    }
}

/// The waveform of the song on Perform: which song folder it is for, and
/// how far its read is.
#[derive(Debug, Default)]
pub struct MapWaveform {
    shown: Option<Shown>,
}

/// The song folder on show and its waveform.
#[derive(Debug)]
struct Shown {
    folder: PathBuf,
    state: State,
}

#[derive(Debug)]
enum State {
    /// Its cached overview is being read.
    Reading {
        ticket: u64,
    },
    /// It has no cached overview: one is built once the transport is
    /// stopped or paused.
    Missing,
    /// Its overview is being built from its stems; dropping the run
    /// cancels the build.
    Building(WaveformRun),
    Ready(SongWave),
    /// It has no waveform: the map shows its blocks.
    Failed,
}

/// Whether the overview may be built from the stems while the transport
/// is in `transport`: stopped or paused, never while it plays or ramps.
fn builds_while(transport: TransportState) -> bool {
    matches!(transport, TransportState::Stopped | TransportState::Paused)
}

impl MapWaveform {
    /// No song on show.
    pub fn new() -> Self {
        Self::default()
    }

    /// The song on show is in `folder`, or none is (`None`: nothing open,
    /// or a setlist's click-only tile), and its transport is in
    /// `transport`. Returns the job to run off the UI thread, if any: for a
    /// song new on show, the read of its cached overview; for one with no
    /// cache, the build of its overview, once the transport is stopped or
    /// paused. A build under way is cancelled once it plays or ramps, and
    /// another song than the last cancels the job of that one.
    pub fn show(
        &mut self,
        folder: Option<&Path>,
        transport: TransportState,
    ) -> Option<SongWaveJob> {
        let Some(folder) = folder else {
            // Dropping the run of the song shown last cancels its build.
            self.shown = None;
            return None;
        };
        let builds = builds_while(transport);
        let Some(shown) = self.shown.as_mut().filter(|shown| shown.folder == folder) else {
            let ticket = next_ticket();
            self.shown = Some(Shown {
                folder: folder.to_owned(),
                state: State::Reading { ticket },
            });
            return Some(SongWaveJob {
                ticket,
                folder: folder.to_owned(),
                step: Step::Cache,
            });
        };
        match shown.state {
            State::Missing if builds => {
                let (waveform, run) = WaveformRun::start(folder.to_owned());
                let ticket = run.ticket;
                shown.state = State::Building(run);
                Some(SongWaveJob {
                    ticket,
                    folder: folder.to_owned(),
                    step: Step::Build(waveform),
                })
            }
            State::Building(_) if !builds => {
                // Dropping the run cancels the build; the next stop starts
                // it again.
                shown.state = State::Missing;
                None
            }
            _ => None,
        }
    }

    /// A song or setlist opened: its `song.json` may have changed, so the
    /// song on show reads its waveform again on its next
    /// [`Self::show`]; a read under way is cancelled.
    pub fn forget(&mut self) {
        self.shown = None;
    }

    /// What a job gave; one for a song no longer on show, or replaced by a
    /// newer one (a build cancelled by play included), changes nothing.
    /// Returns whether the map changes: the waveform of the song on show
    /// is read. A song with no cache waits for a build ([`Self::show`]);
    /// one whose waveform cannot be read keeps the map as it is, and is
    /// not read again while it shows.
    pub fn finish(&mut self, done: SongWaveDone) -> bool {
        let Some(shown) = &mut self.shown else {
            return false;
        };
        let ours = match &shown.state {
            State::Reading { ticket } => *ticket == done.ticket,
            State::Building(run) => run.ticket == done.ticket,
            State::Missing | State::Ready(_) | State::Failed => false,
        };
        if !ours {
            return false;
        }
        match done.result {
            Ok(wave) => {
                shown.state = State::Ready(wave);
                true
            }
            Err(SongWaveError::NotCached | SongWaveError::Overview(OverviewError::Cancelled)) => {
                shown.state = State::Missing;
                false
            }
            Err(_) => {
                shown.state = State::Failed;
                false
            }
        }
    }

    /// The waveform of the song in `folder`, when it is the song on show
    /// and its waveform is read.
    pub fn wave(&self, folder: Option<&Path>) -> Option<&SongWave> {
        let shown = self.shown.as_ref()?;
        if Some(shown.folder.as_path()) != folder {
            return None;
        }
        match &shown.state {
            State::Ready(wave) => Some(wave),
            State::Reading { .. } | State::Missing | State::Building(_) | State::Failed => None,
        }
    }
}
