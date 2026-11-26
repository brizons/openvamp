//! Level meters for a shell to draw (T6b): the peaks the engine reports
//! (`engine::live::Meters`), turned on the control thread into a level that
//! rises at once and falls at [`METER_DECAY_DB_PER_SECOND`], and a peak hold
//! that stays [`METER_HOLD`] and then falls as fast.

use std::time::{Duration, Instant};

use domain::Bus;
use engine::live::{MAX_METERED_STEMS, Meters};

/// How fast a meter's level falls once the signal drops: 20 dB a second.
pub const METER_DECAY_DB_PER_SECOND: f32 = 20.0;

/// How long a meter holds its highest peak before it starts falling, at
/// [`METER_DECAY_DB_PER_SECOND`] too, never below the level.
pub const METER_HOLD: Duration = Duration::from_secs(1);

/// Below this (−100 dBFS) a meter reads silence: exactly 0.0.
pub const METER_FLOOR: f32 = 1e-5;

/// A level meter as a shell draws it, left and right, as linear amplitudes
/// (1.0 is full scale; dBFS is `20 log10`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Meter {
    /// The level: the peak at once, then falling at
    /// [`METER_DECAY_DB_PER_SECOND`].
    pub level: [f32; 2],
    /// The highest recent peak: held [`METER_HOLD`], then falling at the
    /// same rate, never below [`Self::level`].
    pub hold: [f32; 2],
}

/// The ballistics of one meter: what it shows from the peaks it is given
/// and the time between them. A session keeps one per meter and updates
/// them at every poll.
#[derive(Debug, Clone, Copy, Default)]
pub struct MeterBallistics {
    meter: Meter,
    /// Time since each side's hold was last set.
    held: [Duration; 2],
}

impl MeterBallistics {
    /// The meter after `elapsed` more time with `peak` as the newest peak
    /// (absolute sample values, left and right).
    pub fn update(&mut self, peak: [f32; 2], elapsed: Duration) -> Meter {
        let fall =
            |time: Duration| 10f32.powf(-METER_DECAY_DB_PER_SECOND * time.as_secs_f32() / 20.0);
        let floor = |value: f32| if value < METER_FLOOR { 0.0 } else { value };
        for side in 0..2 {
            let peak = if peak[side].is_finite() {
                peak[side].abs()
            } else {
                0.0
            };
            let level = floor(peak.max(self.meter.level[side] * fall(elapsed)));
            let before = self.held[side];
            let after = before + elapsed;
            let mut hold = self.meter.hold[side];
            if after > METER_HOLD {
                hold *= fall(after - before.max(METER_HOLD));
            }
            if peak >= hold {
                hold = peak;
                self.held[side] = Duration::ZERO;
            } else {
                self.held[side] = after;
            }
            self.meter.level[side] = level;
            self.meter.hold[side] = floor(hold.max(level));
        }
        self.meter
    }

    /// What the meter shows.
    pub fn meter(&self) -> Meter {
        self.meter
    }
}

/// The meters of a session: one per stem (up to [`MAX_METERED_STEMS`]), per
/// bus and the master.
#[derive(Debug, Clone)]
pub(crate) struct SessionMeters {
    stems: [MeterBallistics; MAX_METERED_STEMS],
    buses: [MeterBallistics; Bus::ALL.len()],
    master: MeterBallistics,
    /// When they were last updated.
    at: Option<Instant>,
}

impl SessionMeters {
    pub(crate) fn new() -> Self {
        Self {
            stems: [MeterBallistics::default(); MAX_METERED_STEMS],
            buses: [MeterBallistics::default(); Bus::ALL.len()],
            master: MeterBallistics::default(),
            at: None,
        }
    }

    /// Takes the peaks `peaks` the engine reported at `now`.
    pub(crate) fn update(&mut self, peaks: &Meters, now: Instant) {
        let elapsed = self
            .at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        self.at = Some(now);
        for (meter, peak) in self.stems.iter_mut().zip(&peaks.stems) {
            meter.update(*peak, elapsed);
        }
        for (meter, peak) in self.buses.iter_mut().zip(&peaks.buses) {
            meter.update(*peak, elapsed);
        }
        self.master.update(peaks.master, elapsed);
    }

    /// Another item plays: its stems start from silence; the buses and the
    /// master go on.
    pub(crate) fn new_stems(&mut self) {
        self.stems = [MeterBallistics::default(); MAX_METERED_STEMS];
    }

    /// The meter of stem `stem`; `None` past [`MAX_METERED_STEMS`].
    pub(crate) fn stem(&self, stem: usize) -> Option<Meter> {
        self.stems.get(stem).map(MeterBallistics::meter)
    }

    /// The bus meters, in [`Bus::ALL`] order.
    pub(crate) fn buses(&self) -> [Meter; Bus::ALL.len()] {
        self.buses.map(|meter| meter.meter())
    }

    /// The master meter.
    pub(crate) fn master(&self) -> Meter {
        self.master.meter()
    }
}
