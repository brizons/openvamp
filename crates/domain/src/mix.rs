//! Mix format v1: the mix of each arrangement, stored in `mix.json` beside
//! `song.json` (F7.1, F7.2, F7.5).
//!
//! Each stem of an arrangement has a volume, a mute, a solo, and optionally a
//! bus that replaces the one the rules table gives it ([`crate::bus_for_stem`]).
//! Every entry is optional: a missing arrangement, stem or field means unity
//! volume, unmuted, unsoloed and the rules-table bus, so a song without a
//! `mix.json` plays exactly as before. `song.json` is never rewritten.
//!
//! ```json
//! {
//!   "formatVersion": 1,
//!   "arrangements": {
//!     "default": {
//!       "stems": {
//!         "drums": { "volumeDb": -3.5, "mute": false, "solo": false, "bus": "drums" },
//!         "click": { "volumeDb": "-inf", "mute": true, "solo": false }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! `arrangements` is keyed by arrangement id and `stems` by stem id. A volume
//! is a number of decibels relative to unity, at most [`MAX_VOLUME_DB`], or
//! the string `"-inf"` for silence. `bus` is a [`Bus::id`].
//!
//! Like the song package, the file is plain JSON that other tools may write,
//! so [`MixFile::validate`] reports every problem at once, unknown bus ids
//! included, instead of failing on the first.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, DeserializeOwned, Visitor};
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::bus::{Bus, bus_for_stem};
use crate::package::{SongPackage, Stem, VersionHeader};

/// The only `mix.json` `formatVersion` this crate reads. Mix files, song
/// packages and setlists are versioned separately.
pub const MIX_FORMAT_VERSION: u32 = 1;

/// The loudest accepted volume, in dB above unity.
///
/// **Assumption:** +12 dB, the top of a common console fader.
pub const MAX_VOLUME_DB: f64 = 12.0;

/// A fader level in decibels relative to unity (0 dB), down to silence
/// ([`Volume::SILENT`], −∞ dB). Files store it as a number, or `"-inf"` for
/// silence.
///
/// A volume is valid when it is silent or a number of dB up to
/// [`MAX_VOLUME_DB`]; validation reports any other value.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Volume(f64);

impl Volume {
    /// 0 dB: the stem plays as recorded.
    pub const UNITY: Volume = Volume(0.0);

    /// −∞ dB: silence.
    pub const SILENT: Volume = Volume(f64::NEG_INFINITY);

    /// The volume `db` decibels from unity; `f64::NEG_INFINITY` is silence.
    pub const fn from_db(db: f64) -> Volume {
        Volume(db)
    }

    /// Decibels from unity; `f64::NEG_INFINITY` when silent.
    pub const fn db(self) -> f64 {
        self.0
    }

    /// Whether the volume is silent or a number of dB up to [`MAX_VOLUME_DB`].
    pub fn is_valid(self) -> bool {
        self.0 == f64::NEG_INFINITY || (self.0.is_finite() && self.0 <= MAX_VOLUME_DB)
    }

    /// The linear amplitude factor: exactly 1.0 at [`Volume::UNITY`], 0.0 when
    /// silent. An invalid volume never reaches the audio as such: NaN gives
    /// 0.0 and anything louder than [`MAX_VOLUME_DB`] gives the gain of
    /// [`MAX_VOLUME_DB`].
    pub fn gain(self) -> f64 {
        if self.0 == 0.0 {
            1.0
        } else if self.0.is_nan() || self.0 == f64::NEG_INFINITY {
            0.0
        } else {
            10f64.powf(self.0.min(MAX_VOLUME_DB) / 20.0)
        }
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::UNITY
    }
}

/// How files spell [`Volume::SILENT`].
const SILENT_TEXT: &str = "-inf";

impl Serialize for Volume {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0 == f64::NEG_INFINITY {
            serializer.serialize_str(SILENT_TEXT)
        } else if self.0.is_finite() {
            serializer.serialize_f64(self.0)
        } else {
            Err(S::Error::custom(format!(
                "volume {} dB cannot be written; a volume is \"{SILENT_TEXT}\" or a number of dB",
                self.0
            )))
        }
    }
}

impl<'de> Deserialize<'de> for Volume {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(VolumeVisitor)
    }
}

struct VolumeVisitor;

impl Visitor<'_> for VolumeVisitor {
    type Value = Volume;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a number of decibels or \"{SILENT_TEXT}\"")
    }

    fn visit_f64<E: de::Error>(self, db: f64) -> Result<Volume, E> {
        Ok(Volume(db))
    }

    fn visit_i64<E: de::Error>(self, db: i64) -> Result<Volume, E> {
        Ok(Volume(db as f64))
    }

    fn visit_u64<E: de::Error>(self, db: u64) -> Result<Volume, E> {
        Ok(Volume(db as f64))
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Volume, E> {
        if text == SILENT_TEXT {
            Ok(Volume::SILENT)
        } else {
            Err(E::invalid_value(de::Unexpected::Str(text), &self))
        }
    }
}

/// The mix of one stem in one arrangement. Every field has a default, so a
/// missing field or entry is unity, unmuted, unsoloed, on the rules-table bus.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StemMix {
    /// Fader level, stored as `volumeDb`.
    #[serde(rename = "volumeDb", default)]
    pub volume: Volume,
    /// Whether the stem is muted.
    #[serde(default)]
    pub mute: bool,
    /// Whether the stem is soloed.
    #[serde(default)]
    pub solo: bool,
    /// The [`Bus::id`] of the bus the user sent the stem to (F7.2), or `None`
    /// for the rules-table bus. Kept as text so [`MixFile::validate`] can
    /// report an unknown id with every other problem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
}

impl StemMix {
    /// The bus the user sent the stem to, when `bus` names one of the buses.
    pub fn bus_override(&self) -> Option<Bus> {
        self.bus.as_deref().and_then(Bus::from_id)
    }

    /// Sends the stem to `bus`, or back to its rules-table bus with `None`.
    pub fn set_bus(&mut self, bus: Option<Bus>) {
        self.bus = bus.map(|bus| bus.id().to_owned());
    }

    /// Whether every field has its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The mix of one arrangement: the stems whose mix differs from the default,
/// keyed by stem id.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ArrangementMix {
    /// Stem mixes by stem id; a stem without an entry has the default mix.
    #[serde(default)]
    pub stems: BTreeMap<String, StemMix>,
}

impl ArrangementMix {
    /// The mix of stem `id`: its entry, or the default mix.
    pub fn stem(&self, id: &str) -> StemMix {
        self.stems.get(id).cloned().unwrap_or_default()
    }

    /// Stores the mix of stem `id`; a default mix removes the entry.
    pub fn set_stem(&mut self, id: impl Into<String>, mix: StemMix) {
        let id = id.into();
        if mix.is_default() {
            self.stems.remove(&id);
        } else {
            self.stems.insert(id, mix);
        }
    }

    /// The bus `stem` plays through: the user's bus when its entry names one
    /// of the buses, else the rules-table bus ([`bus_for_stem`]).
    pub fn bus_of(&self, stem: &Stem) -> Bus {
        self.stems
            .get(&stem.id)
            .and_then(StemMix::bus_override)
            .unwrap_or_else(|| bus_for_stem(stem))
    }

    /// Whether every stem has the default mix.
    pub fn is_default(&self) -> bool {
        self.stems.values().all(StemMix::is_default)
    }
}

/// A `mix.json` file, exactly as stored: the mixes of a song's arrangements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixFile {
    /// File format version; must be [`MIX_FORMAT_VERSION`].
    pub format_version: u32,
    /// Arrangement mixes by arrangement id; an arrangement without an entry
    /// has the default mix.
    #[serde(default)]
    pub arrangements: BTreeMap<String, ArrangementMix>,
}

impl Default for MixFile {
    fn default() -> Self {
        Self::new()
    }
}

impl MixFile {
    /// A file with no mixes, at [`MIX_FORMAT_VERSION`].
    pub fn new() -> Self {
        Self {
            format_version: MIX_FORMAT_VERSION,
            arrangements: BTreeMap::new(),
        }
    }

    /// Parses a `mix.json` file. `formatVersion` is checked first, so a newer
    /// file reports its version instead of a shape mismatch. Only the JSON
    /// shape is checked here; call [`MixFile::validate`] for the rules.
    pub fn from_json(json: &str) -> Result<Self, MixerFileError> {
        parse(json, MixerFile::Mix, MIX_FORMAT_VERSION)
    }

    /// Writes the file as pretty-printed JSON. A volume that is neither silent
    /// nor a finite number of dB is refused, since JSON cannot hold it.
    pub fn to_json(&self) -> Result<String, MixerFileError> {
        write(self, MixerFile::Mix)
    }

    /// The mix of arrangement `id`: its entry, or the default mix.
    pub fn arrangement(&self, id: &str) -> ArrangementMix {
        self.arrangements.get(id).cloned().unwrap_or_default()
    }

    /// Stores the mix of arrangement `id`; a default mix removes the entry,
    /// which is how "Reset to song mix" forgets an arrangement's mix.
    pub fn set_arrangement(&mut self, id: impl Into<String>, mix: ArrangementMix) {
        let id = id.into();
        if mix.is_default() {
            self.arrangements.remove(&id);
        } else {
            self.arrangements.insert(id, mix);
        }
    }

    /// Checks the file against the song it sits beside and returns every
    /// broken rule at once, in file order: arrangements and stems sorted by
    /// id, and for each stem its id, volume and bus.
    pub fn validate(&self, package: &SongPackage) -> Result<(), Vec<MixValidationError>> {
        let mut errors = Vec::new();
        if self.format_version != MIX_FORMAT_VERSION {
            errors.push(MixValidationError::UnsupportedFormatVersion {
                found: self.format_version,
            });
        }
        for (arrangement, mix) in &self.arrangements {
            if !package.arrangements.iter().any(|a| &a.id == arrangement) {
                errors.push(MixValidationError::UnknownArrangement {
                    arrangement: arrangement.clone(),
                });
            }
            for (stem, stem_mix) in &mix.stems {
                if !package.stems.iter().any(|s| &s.id == stem) {
                    errors.push(MixValidationError::UnknownStem {
                        arrangement: arrangement.clone(),
                        stem: stem.clone(),
                    });
                }
                if !stem_mix.volume.is_valid() {
                    errors.push(MixValidationError::VolumeOutOfRange {
                        arrangement: arrangement.clone(),
                        stem: stem.clone(),
                        db: stem_mix.volume.db(),
                    });
                }
                if let Some(bus) = &stem_mix.bus
                    && Bus::from_id(bus).is_none()
                {
                    errors.push(MixValidationError::UnknownBus {
                        arrangement: arrangement.clone(),
                        stem: stem.clone(),
                        bus: bus.clone(),
                    });
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// A rule a `mix.json` file breaks; [`MixFile::validate`] reports all of them
/// at once.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum MixValidationError {
    /// `formatVersion` is not [`MIX_FORMAT_VERSION`].
    #[error(
        "unsupported formatVersion {found}; this version reads formatVersion {MIX_FORMAT_VERSION}"
    )]
    UnsupportedFormatVersion {
        /// The version in the file.
        found: u32,
    },
    /// The song has no arrangement with this id.
    #[error("mix.json has a mix for arrangement \"{arrangement}\", which the song does not have")]
    UnknownArrangement {
        /// The unknown arrangement id.
        arrangement: String,
    },
    /// The song has no stem with this id.
    #[error("arrangement \"{arrangement}\" mixes stem \"{stem}\", which the song does not have")]
    UnknownStem {
        /// Arrangement id.
        arrangement: String,
        /// The unknown stem id.
        stem: String,
    },
    /// A volume is not silent nor a number of dB up to [`MAX_VOLUME_DB`].
    #[error(
        "arrangement \"{arrangement}\" stem \"{stem}\" has volume {db} dB; a volume is \"-inf\" (silent) or a number of dB up to +{MAX_VOLUME_DB}"
    )]
    VolumeOutOfRange {
        /// Arrangement id.
        arrangement: String,
        /// Stem id.
        stem: String,
        /// The rejected volume in dB.
        db: f64,
    },
    /// A stem is sent to a bus id that is not one of the buses.
    #[error(
        "arrangement \"{arrangement}\" sends stem \"{stem}\" to unknown bus \"{bus}\"; the buses are {}",
        bus_ids()
    )]
    UnknownBus {
        /// Arrangement id.
        arrangement: String,
        /// Stem id.
        stem: String,
        /// The unknown bus id.
        bus: String,
    },
}

fn bus_ids() -> String {
    Bus::ALL.map(Bus::id).join(", ")
}

/// Which mixer file an error comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerFile {
    /// A song's `mix.json`.
    Mix,
    /// The global bus settings.
    BusSettings,
    /// The per-device output routing.
    Routing,
}

impl fmt::Display for MixerFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Mix => "mix.json",
            Self::BusSettings => "bus settings",
            Self::Routing => "routing",
        })
    }
}

/// A mixer file (`mix.json`, the bus settings, or the routing) could not be
/// parsed or written.
#[derive(Debug, Error)]
pub enum MixerFileError {
    /// The file declares a `formatVersion` this crate cannot read; the rest of
    /// its shape is not looked at.
    #[error(
        "{file}: unsupported formatVersion {found}; this version reads formatVersion {supported}"
    )]
    UnsupportedFormatVersion {
        /// The file.
        file: MixerFile,
        /// The version in the file.
        found: u32,
        /// The version this crate reads.
        supported: u32,
    },
    /// The JSON is malformed, does not have the v1 shape, or holds a value
    /// JSON cannot (when writing).
    #[error("{file}: {source}")]
    Json {
        /// The file.
        file: MixerFile,
        /// What serde_json reported.
        source: serde_json::Error,
    },
}

/// Parses a versioned mixer file, checking `formatVersion` before the shape.
pub(crate) fn parse<T: DeserializeOwned>(
    json: &str,
    file: MixerFile,
    supported: u32,
) -> Result<T, MixerFileError> {
    let json_error = |source| MixerFileError::Json { file, source };
    let header: VersionHeader = serde_json::from_str(json).map_err(json_error)?;
    if header.format_version != supported {
        return Err(MixerFileError::UnsupportedFormatVersion {
            file,
            found: header.format_version,
            supported,
        });
    }
    serde_json::from_str(json).map_err(json_error)
}

/// Writes a mixer file as pretty-printed JSON.
pub(crate) fn write<T: Serialize>(value: &T, file: MixerFile) -> Result<String, MixerFileError> {
    serde_json::to_string_pretty(value).map_err(|source| MixerFileError::Json { file, source })
}
