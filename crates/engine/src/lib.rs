//! Offline renderer and live player for the multitrack player.
//!
//! [`render_offline`] plays a [`RenderPlan`] compiled by the `domain` crate
//! against in-memory stems and returns interleaved stereo `f32` audio. The
//! crate does no I/O: the library layer decodes the stems and writes the
//! result. Rendering is deterministic: the same plan, stems, and settings give
//! bit-identical output on every run.
//!
//! [`live::Player`] plays the same song in real time, buffer by buffer, and
//! takes live commands (jumps, loops, fade, panic) from a
//! [`live::Controller`]. With no live command it produces the same samples as
//! [`render_offline`] for the compiled plan. It mixes through buses with
//! per-stem gains that ramp, and routes each bus to 1 to 32 output channels
//! ([`live::Routes`]); with unity gains and every bus on outputs 1 and 2 its
//! outputs 1 and 2 still hold exactly [`render_offline`]'s samples. A
//! [`live::Deck`] plays two players for a setlist and hands over from one to
//! the next (AutoLink, Crossfade) on every channel. The offline renderer
//! stays stereo.
//!
//! Guide cues ([`domain::CueEvent`]) are not rendered yet: no voice samples
//! exist, so the plan's cues are ignored. A `guide` stem plays as plain audio.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use domain::{ClickLevel, RenderPlan, StemRole};

use crate::click::ClickBursts;

mod click;
pub mod live;
mod mix;

/// Default click levels, in the order Downbeat, Beat, HalfBeat, QuarterBeat:
/// beats on, subdivisions off.
pub const DEFAULT_CLICK_LEVELS: [f32; 4] = [1.0, 0.8, 0.0, 0.0];

/// One decoded stem.
#[derive(Debug, Clone, PartialEq)]
pub struct StemAudio {
    /// Stem id, as in the song package.
    pub id: String,
    /// What the stem is used for; a `click` stem turns the generated click off.
    pub role: StemRole,
    /// Number of channels: 1 (mono) or 2 (stereo).
    pub channels: u16,
    /// Interleaved samples, `frames * channels` of them.
    pub samples: Vec<f32>,
    /// Number of sample frames.
    pub frames: u64,
}

/// How the stems and the generated click are mixed.
#[derive(Debug, Clone, PartialEq)]
pub struct MixSettings {
    /// Gain per stem id; a stem without an entry plays at 1.0.
    pub stem_gains: BTreeMap<String, f32>,
    /// Gain applied to the whole mix, generated click included.
    pub master_gain: f32,
    /// The generated click.
    pub click: ClickSettings,
}

impl Default for MixSettings {
    fn default() -> Self {
        Self {
            stem_gains: BTreeMap::new(),
            master_gain: 1.0,
            click: ClickSettings::default(),
        }
    }
}

impl MixSettings {
    /// Gain of the stem `id`: its entry in [`Self::stem_gains`], or 1.0.
    pub fn stem_gain(&self, id: &str) -> f32 {
        self.stem_gains.get(id).copied().unwrap_or(1.0)
    }
}

/// The generated click (F3.1, F3.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickSettings {
    /// Whether the click is generated. It never is when the song has a
    /// `click` stem: the recorded click replaces it.
    pub enabled: bool,
    /// Level of each click level, in the order Downbeat, Beat, HalfBeat,
    /// QuarterBeat. A level of 0.0 emits nothing.
    pub levels: [f32; 4],
}

impl Default for ClickSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            levels: DEFAULT_CLICK_LEVELS,
        }
    }
}

impl ClickSettings {
    /// Level of `level`.
    pub fn level(&self, level: ClickLevel) -> f32 {
        self.levels[level_index(level)]
    }
}

/// Position of `level` in [`ClickSettings::levels`].
pub(crate) fn level_index(level: ClickLevel) -> usize {
    match level {
        ClickLevel::Downbeat => 0,
        ClickLevel::Beat => 1,
        ClickLevel::HalfBeat => 2,
        ClickLevel::QuarterBeat => 3,
    }
}

/// Why a plan could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// A stem has neither 1 nor 2 channels.
    UnsupportedChannels {
        /// Stem id.
        id: String,
        /// Its channel count.
        channels: u16,
    },
    /// A stem's sample count is not `frames * channels`.
    SampleCountMismatch {
        /// Stem id.
        id: String,
        /// Declared frame count.
        frames: u64,
        /// Channel count.
        channels: u16,
        /// Samples present.
        samples: usize,
    },
    /// [`MixSettings::stem_gains`] names a stem that is not rendered.
    UnknownStemGain {
        /// The id with a gain but no stem.
        id: String,
    },
    /// The output does not fit in memory on this platform.
    TooLong {
        /// Plan length in frames.
        frames: u64,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedChannels { id, channels } => write!(
                f,
                "stem \"{id}\" has {channels} channels; only mono and stereo stems are supported"
            ),
            Self::SampleCountMismatch {
                id,
                frames,
                channels,
                samples,
            } => write!(
                f,
                "stem \"{id}\" has {samples} samples, but {frames} frames of {channels} channels need {}",
                frames.saturating_mul(u64::from(*channels))
            ),
            Self::UnknownStemGain { id } => {
                write!(
                    f,
                    "a gain is set for stem \"{id}\", but no stem has that id"
                )
            }
            Self::TooLong { frames } => write!(
                f,
                "the plan is {frames} frames long; the stereo output does not fit in memory"
            ),
        }
    }
}

impl Error for RenderError {}

/// Renders `plan` with `stems` into interleaved stereo audio of exactly
/// `plan.length` frames.
///
/// Every stem plays every segment (`click` and `guide` stems too, as plain
/// audio); then the generated click is added when [`generates_click`] holds,
/// and the master gain is applied. The output and every table are allocated
/// before the render loop starts, and the loop sums in a fixed order (stems in
/// slice order, segments in plan order, then clicks), so the output is
/// bit-identical across runs. With unity gains and no jump seams the output is
/// exactly the plain sum of the stems.
pub fn render_offline(
    plan: &RenderPlan,
    stems: &[StemAudio],
    settings: &MixSettings,
) -> Result<Vec<f32>, RenderError> {
    check_stems(stems)?;
    check_gains(stems, settings)?;
    let too_long = RenderError::TooLong {
        frames: plan.length,
    };
    let samples = usize::try_from(plan.length)
        .ok()
        .and_then(|frames| frames.checked_mul(2))
        .ok_or(too_long.clone())?;
    // Everything the render loop needs is allocated here. A plan too long for
    // the available memory is an error, not an abort.
    let mut output = Vec::new();
    output.try_reserve_exact(samples).map_err(|_| too_long)?;
    output.resize(samples, 0.0f32);
    let bursts = generates_click(stems, settings).then(|| ClickBursts::new(plan.sample_rate));

    for stem in stems {
        let gain = settings.stem_gain(&stem.id);
        for segment in &plan.segments {
            mix::add_segment(&mut output, stem, gain, segment);
        }
    }
    if let Some(bursts) = &bursts {
        for click in &plan.clicks {
            bursts.add(
                &mut output,
                click.at,
                click.level,
                settings.click.level(click.level),
            );
        }
    }
    for sample in &mut output {
        *sample *= settings.master_gain;
    }
    Ok(output)
}

pub(crate) fn check_stems(stems: &[StemAudio]) -> Result<(), RenderError> {
    for stem in stems {
        if !(1..=2).contains(&stem.channels) {
            return Err(RenderError::UnsupportedChannels {
                id: stem.id.clone(),
                channels: stem.channels,
            });
        }
        let expected = stem.frames.checked_mul(u64::from(stem.channels));
        if expected != Some(stem.samples.len() as u64) {
            return Err(RenderError::SampleCountMismatch {
                id: stem.id.clone(),
                frames: stem.frames,
                channels: stem.channels,
                samples: stem.samples.len(),
            });
        }
    }
    Ok(())
}

fn check_gains(stems: &[StemAudio], settings: &MixSettings) -> Result<(), RenderError> {
    match settings
        .stem_gains
        .keys()
        .find(|id| !stems.iter().any(|stem| &stem.id == *id))
    {
        Some(id) => Err(RenderError::UnknownStemGain { id: id.clone() }),
        None => Ok(()),
    }
}

/// Whether [`render_offline`] generates a click: the click is enabled and no
/// stem has the `click` role.
pub fn generates_click(stems: &[StemAudio], settings: &MixSettings) -> bool {
    settings.click.enabled && !stems.iter().any(|stem| stem.role == StemRole::Click)
}

/// Number of generated click ticks [`render_offline`] makes audible: the plan's
/// clicks whose level is not 0.0, or none when the click is not generated.
pub fn audible_click_count(
    plan: &RenderPlan,
    stems: &[StemAudio],
    settings: &MixSettings,
) -> usize {
    if !generates_click(stems, settings) {
        return 0;
    }
    plan.clicks
        .iter()
        .filter(|click| settings.click.level(click.level) != 0.0)
        .count()
}
