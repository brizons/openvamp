//! Global bus settings and the effective gain of every stem (F7.1, F7.3,
//! F7.5).
//!
//! Each bus has a volume and a mute, and there is one master volume. They are
//! global (F7.5): every song plays through the same [`BusSettings`], so they
//! carry from song to song in a setlist, while each arrangement keeps its own
//! [`ArrangementMix`]. The app stores the settings in its configuration
//! folder:
//!
//! ```json
//! {
//!   "formatVersion": 1,
//!   "buses": {
//!     "drums": { "volumeDb": -2.0, "mute": false },
//!     "vocals": { "volumeDb": 0.0, "mute": true }
//!   },
//!   "masterVolumeDb": -1.5
//! }
//! ```
//!
//! `buses` is keyed by [`Bus::id`]; a missing bus is at unity and unmuted, and
//! a missing master volume is unity. Volumes are written as in `mix.json`
//! ([`Volume`]). The app writes this file itself, so an unknown bus id is a
//! shape error of [`BusSettings::from_json`].
//!
//! [`effective_gains`] turns a mix and the bus settings into one linear gain
//! per stem, which the engine ramps to (F7.4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::Bus;
use crate::mix::{
    ArrangementMix, MAX_VOLUME_DB, MixerFile, MixerFileError, StemMix, Volume, parse, write,
};
use crate::package::{Stem, StemRole};

/// The only bus-settings `formatVersion` this crate reads.
pub const BUS_SETTINGS_FORMAT_VERSION: u32 = 1;

/// The volume and mute of one bus.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct BusLevel {
    /// Fader level, stored as `volumeDb`; unity by default.
    #[serde(rename = "volumeDb", default)]
    pub volume: Volume,
    /// Whether the bus is muted: every stem in it is silent.
    #[serde(default)]
    pub mute: bool,
}

/// The global bus settings, exactly as stored (see the [module docs](self)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusSettings {
    /// File format version; must be [`BUS_SETTINGS_FORMAT_VERSION`].
    pub format_version: u32,
    /// Bus levels by bus; a bus without an entry is at unity and unmuted.
    #[serde(default)]
    pub buses: BTreeMap<Bus, BusLevel>,
    /// The master volume, applied to every stem and to the generated click;
    /// stored as `masterVolumeDb`.
    #[serde(rename = "masterVolumeDb", default)]
    pub master_volume: Volume,
}

impl Default for BusSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl BusSettings {
    /// Every bus and the master at unity, unmuted, at
    /// [`BUS_SETTINGS_FORMAT_VERSION`].
    pub fn new() -> Self {
        Self {
            format_version: BUS_SETTINGS_FORMAT_VERSION,
            buses: BTreeMap::new(),
            master_volume: Volume::UNITY,
        }
    }

    /// Parses the bus settings. `formatVersion` is checked first, so a newer
    /// file reports its version instead of a shape mismatch.
    pub fn from_json(json: &str) -> Result<Self, MixerFileError> {
        parse(json, MixerFile::BusSettings, BUS_SETTINGS_FORMAT_VERSION)
    }

    /// Writes the bus settings as pretty-printed JSON. A volume that is
    /// neither silent nor a finite number of dB is refused.
    pub fn to_json(&self) -> Result<String, MixerFileError> {
        write(self, MixerFile::BusSettings)
    }

    /// The level of `bus`: its entry, or unity and unmuted.
    pub fn level(&self, bus: Bus) -> BusLevel {
        self.buses.get(&bus).copied().unwrap_or_default()
    }

    /// Stores the level of `bus`; the default level removes the entry.
    pub fn set_level(&mut self, bus: Bus, level: BusLevel) {
        if level == BusLevel::default() {
            self.buses.remove(&bus);
        } else {
            self.buses.insert(bus, level);
        }
    }

    /// The gain of everything that plays through `bus` at unity, such as the
    /// generated click and guide cues on [`Bus::ClickGuide`]: 0.0 when the bus
    /// is muted, else the bus volume times the master volume. Exactly 1.0 when
    /// both are at unity.
    pub fn bus_gain(&self, bus: Bus) -> f32 {
        let level = self.level(bus);
        if level.mute {
            0.0
        } else {
            (level.volume.gain() * self.master_volume.gain()) as f32
        }
    }

    /// Checks every volume and returns all broken rules at once.
    pub fn validate(&self) -> Result<(), Vec<BusSettingsError>> {
        let mut errors = Vec::new();
        if self.format_version != BUS_SETTINGS_FORMAT_VERSION {
            errors.push(BusSettingsError::UnsupportedFormatVersion {
                found: self.format_version,
            });
        }
        for (&bus, level) in &self.buses {
            if !level.volume.is_valid() {
                errors.push(BusSettingsError::BusVolumeOutOfRange {
                    bus,
                    db: level.volume.db(),
                });
            }
        }
        if !self.master_volume.is_valid() {
            errors.push(BusSettingsError::MasterVolumeOutOfRange {
                db: self.master_volume.db(),
            });
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// A rule the bus settings break; [`BusSettings::validate`] reports all of
/// them at once.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum BusSettingsError {
    /// `formatVersion` is not [`BUS_SETTINGS_FORMAT_VERSION`].
    #[error(
        "unsupported formatVersion {found}; this version reads formatVersion {BUS_SETTINGS_FORMAT_VERSION}"
    )]
    UnsupportedFormatVersion {
        /// The version in the file.
        found: u32,
    },
    /// A bus volume is not silent nor a number of dB up to [`MAX_VOLUME_DB`].
    #[error(
        "bus {bus} has volume {db} dB; a volume is \"-inf\" (silent) or a number of dB up to +{MAX_VOLUME_DB}"
    )]
    BusVolumeOutOfRange {
        /// The bus.
        bus: Bus,
        /// The rejected volume in dB.
        db: f64,
    },
    /// The master volume is not silent nor a number of dB up to
    /// [`MAX_VOLUME_DB`].
    #[error(
        "the master volume is {db} dB; a volume is \"-inf\" (silent) or a number of dB up to +{MAX_VOLUME_DB}"
    )]
    MasterVolumeOutOfRange {
        /// The rejected volume in dB.
        db: f64,
    },
}

/// What one stem contributes to the output: the bus it plays through and its
/// linear gain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StemGain {
    /// The bus the stem plays through ([`ArrangementMix::bus_of`]).
    pub bus: Bus,
    /// Linear amplitude factor: 0.0 when the stem is silent, exactly 1.0 when
    /// the stem, its bus and the master are at unity.
    pub gain: f32,
}

/// The effective gain and bus of each stem of `stems`, in the same order.
///
/// A stem is silent (gain 0.0) when it is muted, when its bus is muted, or
/// when another stem is soloed and it is not, unless it is a click or guide
/// stem (F7.3): a stem with the click or guide role, or a stem on
/// [`Bus::ClickGuide`]. Otherwise its gain is its volume times its bus volume
/// times the master volume.
///
/// **Assumption (S1):** solo applies to stems only; mute wins over solo, so a
/// muted stem or a stem on a muted bus stays silent even when soloed. Only
/// the solos of stems in `stems` count.
pub fn effective_gains(stems: &[Stem], mix: &ArrangementMix, buses: &BusSettings) -> Vec<StemGain> {
    let mixes: Vec<StemMix> = stems.iter().map(|stem| mix.stem(&stem.id)).collect();
    let any_solo = mixes.iter().any(|stem| stem.solo);
    let master = buses.master_volume.gain();
    stems
        .iter()
        .zip(&mixes)
        .map(|(stem, stem_mix)| {
            let bus = mix.bus_of(stem);
            let level = buses.level(bus);
            let click_or_guide =
                bus == Bus::ClickGuide || matches!(stem.role, StemRole::Click | StemRole::Guide);
            let silent =
                stem_mix.mute || level.mute || (any_solo && !stem_mix.solo && !click_or_guide);
            let gain = if silent {
                0.0
            } else {
                (stem_mix.volume.gain() * level.volume.gain() * master) as f32
            };
            StemGain { bus, gain }
        })
        .collect()
}
