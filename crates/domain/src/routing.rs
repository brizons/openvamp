//! Routing format v1: which outputs of an audio device each bus plays on
//! (F8.2–F8.4), saved per device in the app's configuration folder.
//!
//! A [`Routing`] sends each [`Bus`] to a mono output or a stereo pair of a
//! device with 1 to [`MAX_OUTPUTS`] outputs, counted from 1 as the device
//! labels them. [`Routing::preset`] gives the default routing for an output
//! count (F8.3), and split mode (F8.4) plays click and guide on output 1 and
//! everything else summed to mono on output 2. A [`RoutingFile`] holds the
//! routing of every device the user has edited:
//!
//! ```json
//! {
//!   "formatVersion": 1,
//!   "devices": {
//!     "USB interface": {
//!       "outputs": 8,
//!       "splitMode": false,
//!       "buses": {
//!         "clickGuide": [1],
//!         "drums": [2],
//!         "bass": [3],
//!         "keysPads": [5, 6],
//!         "guitars": [4],
//!         "vocals": [7, 8]
//!       },
//!       "master": [7, 8]
//!     }
//!   }
//! }
//! ```
//!
//! `devices` is keyed by the output device id. A destination is `[n]` for output
//! `n` alone or `[left, right]` for a stereo pair. A bus without a
//! destination plays on outputs 1 and 2 (output 1 on a one-output device);
//! `master` is optional.
//!
//! **Assumption (S2):** a mono output receives the bus's left and right
//! summed, and buses that share an output are summed.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::Bus;
use crate::mix::{MixerFile, MixerFileError, parse, write};

/// The only routing `formatVersion` this crate reads.
pub const ROUTING_FORMAT_VERSION: u32 = 1;

/// The most outputs a routing uses (F8.2); a device with more uses its first
/// [`MAX_OUTPUTS`].
pub const MAX_OUTPUTS: u16 = 32;

/// Where a bus plays: one output or a stereo pair, counted from 1. Stored as
/// `[n]` or `[left, right]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<u16>", into = "Vec<u16>")]
pub enum Destination {
    /// One output, receiving the left and right channels summed.
    Mono(u16),
    /// A stereo pair: the left channel on `left`, the right on `right`.
    Stereo {
        /// Output of the left channel.
        left: u16,
        /// Output of the right channel.
        right: u16,
    },
}

impl Destination {
    /// Outputs 1 and 2 as a stereo pair: where everything played before
    /// routing existed.
    pub const MAIN: Destination = Destination::Stereo { left: 1, right: 2 };

    /// The outputs this destination plays on.
    pub fn outputs(self) -> Vec<u16> {
        self.into()
    }

    /// Whether every output is from 1 to `outputs` and a pair uses two
    /// different outputs.
    pub fn fits(self, outputs: u16) -> bool {
        self.problems(outputs).is_empty()
    }

    /// What keeps the destination from fitting a device with `outputs`
    /// outputs, in order: each output outside 1 to `outputs`, once, then a
    /// pair that uses one output twice. The one rule behind
    /// [`Destination::fits`] and [`Routing::validate`].
    fn problems(self, outputs: u16) -> Vec<DestinationProblem> {
        let out_of_range = |output: u16| {
            (!(1..=outputs).contains(&output)).then_some(DestinationProblem::OutOfRange(output))
        };
        match self {
            Self::Mono(output) => out_of_range(output).into_iter().collect(),
            Self::Stereo { left, right } if left == right => out_of_range(left)
                .into_iter()
                .chain([DestinationProblem::SamePair(left)])
                .collect(),
            Self::Stereo { left, right } => {
                [left, right].into_iter().filter_map(out_of_range).collect()
            }
        }
    }

    /// Where a bus plays when its own destination does not fit a device with
    /// `outputs` outputs: outputs 1 and 2, or output 1 alone.
    fn fallback(outputs: u16) -> Destination {
        if outputs >= 2 {
            Self::MAIN
        } else {
            Self::Mono(1)
        }
    }
}

/// Why a destination does not fit a device ([`Destination::problems`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationProblem {
    /// The output is 0 or above the device's output count.
    OutOfRange(u16),
    /// A stereo pair uses this output for both channels.
    SamePair(u16),
}

impl TryFrom<Vec<u16>> for Destination {
    type Error = String;

    fn try_from(outputs: Vec<u16>) -> Result<Self, Self::Error> {
        match outputs[..] {
            [output] => Ok(Self::Mono(output)),
            [left, right] => Ok(Self::Stereo { left, right }),
            _ => Err(format!(
                "a destination is one output or a stereo pair, such as [3] or [1, 2], not {outputs:?}"
            )),
        }
    }
}

impl From<Destination> for Vec<u16> {
    fn from(destination: Destination) -> Self {
        match destination {
            Destination::Mono(output) => vec![output],
            Destination::Stereo { left, right } => vec![left, right],
        }
    }
}

/// The routing of one device, exactly as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Routing {
    /// The device's output count, from 1 to [`MAX_OUTPUTS`].
    pub outputs: u16,
    /// Split mode (F8.4): click and guide on output 1, everything else summed
    /// to mono on output 2, whatever `buses` says; `buses` is kept for when
    /// split mode is turned off.
    #[serde(default)]
    pub split_mode: bool,
    /// Destination of each bus; a bus without one plays on outputs 1 and 2.
    #[serde(default)]
    pub buses: BTreeMap<Bus, Destination>,
    /// Where the main mix also plays, if anywhere.
    ///
    /// **Assumption:** the main mix is every bus except Click & Guide, after
    /// the bus and master volumes, like the stems side of split mode; no
    /// preset uses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<Destination>,
}

impl Routing {
    /// The default routing for a device with `outputs` outputs (F8.3),
    /// clamped to 1–[`MAX_OUTPUTS`]. In mixer order:
    ///
    /// | Outputs | Click & Guide | Drums | Bass | Keys & Pads | Guitars | Vocals |
    /// |---------|---------------|-------|------|-------------|---------|--------|
    /// | 1       | 1             | 1     | 1    | 1           | 1       | 1      |
    /// | 2       | 1/2           | 1/2   | 1/2  | 1/2         | 1/2     | 1/2    |
    /// | 3       | 1             | 2/3   | 2/3  | 2/3         | 2/3     | 2/3    |
    /// | 4–5     | 1/2           | 3/4   | 3/4  | 3/4         | 3/4     | 3/4    |
    /// | 6–7     | 1             | 2     | 3    | 4           | 5       | 6      |
    /// | 8–32    | 1             | 2     | 3    | 5/6         | 4       | 7/8    |
    ///
    /// With 2 outputs it is plain stereo ([`Routing::stereo`]), split mode
    /// off; split mode (F8.4) stays a switch the user turns on.
    ///
    /// **Assumption (S2), from the design canvas's routing data:** Click &
    /// Guide alone on output 1 for the band's in-ears, and from six outputs
    /// each bus on its own output for front of house; from eight outputs the
    /// wide buses (pads and strings, choir and backing vocals) get a stereo
    /// pair, as the design's larger devices give them. With 3 to 5 outputs,
    /// which the design does not show, the stems share one stereo pair.
    pub fn preset(outputs: u16) -> Routing {
        let outputs = outputs.clamp(1, MAX_OUTPUTS);
        let pair = |left, right| Destination::Stereo { left, right };
        use Destination::Mono;
        let (split_mode, destinations) = match outputs {
            1 => (false, [Mono(1); 6]),
            2 => (false, [Destination::MAIN; 6]),
            3 => (
                false,
                [
                    Mono(1),
                    pair(2, 3),
                    pair(2, 3),
                    pair(2, 3),
                    pair(2, 3),
                    pair(2, 3),
                ],
            ),
            4 | 5 => (
                false,
                [
                    pair(1, 2),
                    pair(3, 4),
                    pair(3, 4),
                    pair(3, 4),
                    pair(3, 4),
                    pair(3, 4),
                ],
            ),
            6 | 7 => (
                false,
                [Mono(1), Mono(2), Mono(3), Mono(4), Mono(5), Mono(6)],
            ),
            _ => (
                false,
                [Mono(1), Mono(2), Mono(3), pair(5, 6), Mono(4), pair(7, 8)],
            ),
        };
        Routing {
            outputs,
            split_mode,
            buses: Bus::ALL.into_iter().zip(destinations).collect(),
            master: None,
        }
    }

    /// Every bus on outputs 1 and 2 (output 1 on a one-output device), split
    /// mode off: the plain stereo output the players had before routing.
    pub fn stereo(outputs: u16) -> Routing {
        let outputs = outputs.clamp(1, MAX_OUTPUTS);
        Routing {
            outputs,
            split_mode: false,
            buses: Bus::ALL
                .into_iter()
                .map(|bus| (bus, Destination::fallback(outputs)))
                .collect(),
            master: None,
        }
    }

    /// Where `bus` plays: in split mode (on two or more outputs) output 1 for
    /// [`Bus::ClickGuide`] and output 2 for the others, else its stored
    /// destination, or outputs 1 and 2 when it has none.
    pub fn destination(&self, bus: Bus) -> Destination {
        if self.split_active() {
            return match bus {
                Bus::ClickGuide => Destination::Mono(1),
                _ => Destination::Mono(2),
            };
        }
        self.buses
            .get(&bus)
            .copied()
            .unwrap_or_else(|| Destination::fallback(self.outputs))
    }

    /// Where the main mix also plays: [`Routing::master`], or nowhere in split
    /// mode, which uses outputs 1 and 2 only.
    pub fn master_destination(&self) -> Option<Destination> {
        if self.split_active() {
            None
        } else {
            self.master
        }
    }

    fn split_active(&self) -> bool {
        self.split_mode && self.outputs >= 2
    }

    /// This routing for a device with `outputs` outputs (clamped to
    /// 1–[`MAX_OUTPUTS`]): every bus destination that does not fit falls back
    /// to outputs 1 and 2 (output 1 on a one-output device), a master
    /// destination that does not fit is dropped, and split mode is turned off
    /// on one output. The result always passes [`Routing::validate`].
    pub fn fit(&self, outputs: u16) -> Routing {
        let outputs = outputs.clamp(1, MAX_OUTPUTS);
        Routing {
            outputs,
            split_mode: self.split_mode && outputs >= 2,
            buses: self
                .buses
                .iter()
                .map(|(&bus, &destination)| {
                    let destination = if destination.fits(outputs) {
                        destination
                    } else {
                        Destination::fallback(outputs)
                    };
                    (bus, destination)
                })
                .collect(),
            master: self.master.filter(|master| master.fits(outputs)),
        }
    }

    /// Checks the routing and returns every broken rule at once: the output
    /// count, split mode, then each bus in mixer order and the master.
    /// Destinations are checked against the outputs the routing can use, at
    /// most [`MAX_OUTPUTS`].
    pub fn validate(&self) -> Result<(), Vec<RoutingError>> {
        let mut errors = Vec::new();
        if !(1..=MAX_OUTPUTS).contains(&self.outputs) {
            errors.push(RoutingError::OutputCountOutOfRange {
                outputs: self.outputs,
            });
        }
        if self.split_mode && self.outputs < 2 {
            errors.push(RoutingError::SplitModeNeedsTwoOutputs {
                outputs: self.outputs,
            });
        }
        let usable = self.outputs.min(MAX_OUTPUTS);
        let targets = self
            .buses
            .iter()
            .map(|(&bus, &destination)| (RouteTarget::Bus(bus), destination))
            .chain(self.master.map(|master| (RouteTarget::Master, master)));
        for (target, destination) in targets {
            errors.extend(
                destination
                    .problems(usable)
                    .into_iter()
                    .map(|problem| match problem {
                        DestinationProblem::OutOfRange(output) => RoutingError::OutputOutOfRange {
                            target,
                            output,
                            outputs: usable,
                        },
                        DestinationProblem::SamePair(output) => {
                            RoutingError::SameOutputPair { target, output }
                        }
                    }),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// What a routing error is about: a bus, or the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTarget {
    /// A bus.
    Bus(Bus),
    /// The main mix ([`Routing::master`]).
    Master,
}

impl fmt::Display for RouteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(bus) => write!(f, "bus {bus}"),
            Self::Master => f.write_str("the master"),
        }
    }
}

/// A rule a [`Routing`] breaks; [`Routing::validate`] reports all of them at
/// once.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoutingError {
    /// The output count is 0 or above [`MAX_OUTPUTS`].
    #[error("the routing has {outputs} outputs; a device has 1 to {MAX_OUTPUTS} outputs")]
    OutputCountOutOfRange {
        /// The rejected count.
        outputs: u16,
    },
    /// Split mode is on for a device with fewer than two outputs.
    #[error("split mode needs 2 outputs, but the routing has {outputs}")]
    SplitModeNeedsTwoOutputs {
        /// The device's output count.
        outputs: u16,
    },
    /// A destination names output 0 or an output the device does not have.
    #[error("{target} goes to output {output}, but the device has outputs 1 to {outputs}")]
    OutputOutOfRange {
        /// The bus or the master.
        target: RouteTarget,
        /// The rejected output.
        output: u16,
        /// The outputs the routing can use.
        outputs: u16,
    },
    /// A stereo pair uses the same output twice.
    #[error(
        "{target} goes to the stereo pair {output}/{output}; a pair needs two different outputs"
    )]
    SameOutputPair {
        /// The bus or the master.
        target: RouteTarget,
        /// The repeated output.
        output: u16,
    },
}

/// The routing of every device the user has edited, exactly as stored (see
/// the [module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingFile {
    /// File format version; must be [`ROUTING_FORMAT_VERSION`].
    pub format_version: u32,
    /// Routing by output device id.
    #[serde(default)]
    pub devices: BTreeMap<String, Routing>,
}

impl Default for RoutingFile {
    fn default() -> Self {
        Self::new()
    }
}

impl RoutingFile {
    /// A file with no devices, at [`ROUTING_FORMAT_VERSION`].
    pub fn new() -> Self {
        Self {
            format_version: ROUTING_FORMAT_VERSION,
            devices: BTreeMap::new(),
        }
    }

    /// Parses a routing file. `formatVersion` is checked first, so a newer
    /// file reports its version instead of a shape mismatch.
    pub fn from_json(json: &str) -> Result<Self, MixerFileError> {
        parse(json, MixerFile::Routing, ROUTING_FORMAT_VERSION)
    }

    /// Writes the file as pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, MixerFileError> {
        write(self, MixerFile::Routing)
    }

    /// The routing to play `device` with when it has `outputs` outputs: its
    /// saved routing fitted to those outputs ([`Routing::fit`]), or the preset
    /// for them when the device has none saved.
    pub fn routing(&self, device: &str, outputs: u16) -> Routing {
        self.devices
            .get(device)
            .map(|routing| routing.fit(outputs))
            .unwrap_or_else(|| Routing::preset(outputs))
    }

    /// Saves the routing of `device`.
    pub fn set_routing(&mut self, device: impl Into<String>, routing: Routing) {
        self.devices.insert(device.into(), routing);
    }

    /// Checks every device's routing and returns all broken rules at once, in
    /// device-name order.
    pub fn validate(&self) -> Result<(), Vec<RoutingValidationError>> {
        let mut errors = Vec::new();
        if self.format_version != ROUTING_FORMAT_VERSION {
            errors.push(RoutingValidationError::UnsupportedFormatVersion {
                found: self.format_version,
            });
        }
        for (device, routing) in &self.devices {
            if let Err(device_errors) = routing.validate() {
                errors.extend(device_errors.into_iter().map(|error| {
                    RoutingValidationError::Device {
                        device: device.clone(),
                        error,
                    }
                }));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// A rule a [`RoutingFile`] breaks; [`RoutingFile::validate`] reports all of
/// them at once.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoutingValidationError {
    /// `formatVersion` is not [`ROUTING_FORMAT_VERSION`].
    #[error(
        "unsupported formatVersion {found}; this version reads formatVersion {ROUTING_FORMAT_VERSION}"
    )]
    UnsupportedFormatVersion {
        /// The version in the file.
        found: u32,
    },
    /// A device's routing breaks a rule.
    #[error("device \"{device}\": {error}")]
    Device {
        /// The output device id.
        device: String,
        /// The broken rule.
        error: RoutingError,
    },
}
