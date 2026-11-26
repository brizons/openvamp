//! Level meters (T6b): the peaks the live player measures while it renders,
//! for a shell to draw beside the faders.

use domain::Bus;

/// Most stems the meters follow, in stem order. A package with more still
/// plays every stem, and every one reaches its bus meter and the master
/// meter; only the stems past this many have no meter of their own.
pub const MAX_METERED_STEMS: usize = 64;

/// How long a peak stays in [`Meters`]: the meters keep the peaks of the
/// window under way and of the last whole one, windows of this length
/// counted in output frames, so every peak stays one to two windows and a
/// reader polling every 50 ms or more often (a shell at 30 Hz polls every
/// 33 ms) misses none. The windows do not depend on the buffer size.
pub const METER_WINDOW_MILLISECONDS: u64 = 50;

/// Number of buses: [`Bus::ALL`].
const BUSES: usize = Bus::ALL.len();

/// Peak levels, as absolute sample values (1.0 is full scale), left and
/// right: the highest of every block rendered in the last one to two
/// [`METER_WINDOW_MILLISECONDS`] windows. The meters only read the signal;
/// they change nothing that plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meters {
    /// Each stem as it reaches its bus: after its gain (the mixer's
    /// volume, mute, solo, bus and master), the track fades, and the
    /// player's master gain and transport ramp. In stem order; stems past
    /// the song's and past [`MAX_METERED_STEMS`] read 0.0.
    pub stems: [[f32; 2]; MAX_METERED_STEMS],
    /// Each bus, in [`Bus::ALL`] order: the sum of its stems (and the
    /// generated click on Click & Guide) after the master gain, before the
    /// router, so split mode and a mono output's fold do not change it.
    pub buses: [[f32; 2]; BUSES],
    /// The master: every bus summed, after the master gain, before the
    /// router. It is what the plain stereo routing plays on outputs 1 and 2.
    pub master: [f32; 2],
}

impl Meters {
    /// Every meter at silence.
    pub const SILENT: Meters = Meters {
        stems: [[0.0; 2]; MAX_METERED_STEMS],
        buses: [[0.0; 2]; BUSES],
        master: [0.0; 2],
    };

    /// Raises each meter of `self` to `other`'s, for the first `stems`
    /// stems (the others are silent in both).
    pub(crate) fn raise_to(&mut self, other: &Meters, stems: usize) {
        let pair = |a: &mut [f32; 2], b: &[f32; 2]| {
            a[0] = a[0].max(b[0]);
            a[1] = a[1].max(b[1]);
        };
        for (peak, other) in self.stems[..stems].iter_mut().zip(&other.stems) {
            pair(peak, other);
        }
        for (peak, other) in self.buses.iter_mut().zip(&other.buses) {
            pair(peak, other);
        }
        pair(&mut self.master, &other.master);
    }

    /// Silences the first `stems` stems (the others are silent already),
    /// every bus and the master.
    pub(crate) fn clear(&mut self, stems: usize) {
        self.stems[..stems].fill([0.0; 2]);
        self.buses = [[0.0; 2]; BUSES];
        self.master = [0.0; 2];
    }
}

/// Raises the peak `peak` to `|left|` and `|right|`.
pub(crate) fn raise(peak: &mut [f32; 2], left: f32, right: f32) {
    peak[0] = peak[0].max(left.abs());
    peak[1] = peak[1].max(right.abs());
}

/// The player's meters: the peaks of the window under way and of the last
/// whole one, windows counted in output frames from the player's first, so
/// they do not depend on how the frames are cut into blocks. A block hands
/// in its frames in the two windows that matter once it ends
/// ([`Self::segments`]). Real-time safe: plain arrays, nothing allocated.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MeterWindows {
    window: u64,
    /// Stems with a meter: the player's, at most [`MAX_METERED_STEMS`].
    stems: usize,
    /// Frames of the window under way so far.
    elapsed: u64,
    current: Meters,
    previous: Meters,
}

impl MeterWindows {
    /// Windows of [`METER_WINDOW_MILLISECONDS`] at `sample_rate`, for a
    /// player with `stems` stems.
    pub(crate) fn new(sample_rate: u32, stems: usize) -> Self {
        Self {
            window: (u64::from(sample_rate) * METER_WINDOW_MILLISECONDS / 1_000).max(1),
            stems: stems.min(MAX_METERED_STEMS),
            elapsed: 0,
            current: Meters::SILENT,
            previous: Meters::SILENT,
        }
    }

    /// Where the frames of a block of `n` frames, starting now, go:
    /// `(from, split)`. Frames from `split` on belong to the window under
    /// way once the block ends, frames from `from` to `split` to the window
    /// before it, and earlier frames, more than two windows old by then, to
    /// neither.
    pub(crate) fn segments(&self, n: usize) -> (usize, usize) {
        let end = self.elapsed + n as u64;
        if end < self.window {
            return (0, 0);
        }
        let split = n - (end % self.window) as usize;
        (split.saturating_sub(self.window as usize), split)
    }

    /// A block of `frames` frames ended, its frames of the window before
    /// the one under way peaking at `before` and those of the one under way
    /// at `after` ([`Self::segments`]).
    pub(crate) fn add(&mut self, before: &Meters, after: &Meters, frames: u64) {
        let end = self.elapsed + frames;
        let stems = self.stems;
        match end / self.window {
            0 => self.current.raise_to(after, stems),
            1 => {
                self.previous = self.current;
                self.previous.raise_to(before, stems);
                self.current = *after;
            }
            _ => {
                self.previous = *before;
                self.current = *after;
            }
        }
        self.elapsed = end % self.window;
    }

    /// `frames` silent frames went by (the transport did not run).
    pub(crate) fn silence(&mut self, frames: u64) {
        let end = self.elapsed + frames;
        match end / self.window {
            0 => {}
            1 => {
                self.previous = self.current;
                self.current.clear(self.stems);
            }
            _ => {
                self.previous.clear(self.stems);
                self.current.clear(self.stems);
            }
        }
        self.elapsed = end % self.window;
    }

    /// Stems with a meter.
    pub(crate) fn stems(&self) -> usize {
        self.stems
    }

    /// The meters a reader gets: the peaks of both windows.
    pub(crate) fn published(&self) -> Meters {
        let mut meters = self.previous;
        meters.raise_to(&self.current, self.stems);
        meters
    }
}
