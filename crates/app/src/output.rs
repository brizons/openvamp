//! The audio-output port: what a session needs from an audio device.
//!
//! An adapter implements [`AudioOutput`] for real hardware (`io-cpal`);
//! [`crate::testing::FakeOutput`] implements it for tests. The session asks
//! for one stream at the song's sample rate, with the channels its routing
//! uses, and hands it an [`AudioRenderer`], which the adapter calls from its
//! audio thread.

use std::fmt;
use std::time::Duration;

use thiserror::Error;

use crate::AudioRenderer;

/// Smallest buffer a session opens a device with, in frames (F8.5).
pub const MIN_BUFFER_FRAMES: u32 = 64;

/// Largest buffer a session opens a device with, in frames (F8.5).
pub const MAX_BUFFER_FRAMES: u32 = 1_024;

/// Buffer size a session uses unless the shell chooses one.
pub const DEFAULT_BUFFER_FRAMES: u32 = 256;

/// An audio output device (F8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// What tells the device apart from every other one, even an identical
    /// interface with the same name; it stays the same while the device is
    /// unplugged and plugged back in, as far as the system allows (the
    /// adapter documents how far). [`DeviceChoice`] takes it.
    pub id: String,
    /// Name, for people; two devices can share it.
    pub name: String,
    /// Most output channels the device offers.
    pub output_channels: u16,
    /// Whether it is the system's default output.
    pub is_default: bool,
}

/// Which output device a request means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceChoice {
    /// The device whose [`DeviceInfo::id`] this is. A session pins the
    /// device it opened this way, so it never comes back to another one.
    Id(String),
    /// The device a person named: the one with this id, else the one with
    /// this name, else the one with this name ignoring ASCII case. A name
    /// more than one device has is refused ([`OutputError::AmbiguousDevice`]).
    Named(String),
}

impl DeviceChoice {
    /// The index of the device this choice means among `devices`, given as
    /// `(id, name)` pairs.
    pub fn find(&self, devices: &[(&str, &str)]) -> Result<usize, OutputError> {
        let wanted = match self {
            Self::Id(id) | Self::Named(id) => id.as_str(),
        };
        if let Some(index) = devices.iter().position(|&(id, _)| id == wanted) {
            return Ok(index);
        }
        if let Self::Named(name) = self {
            let exact = |candidate: &str| candidate == name;
            let loose = |candidate: &str| candidate.eq_ignore_ascii_case(name);
            for matches in [&exact as &dyn Fn(&str) -> bool, &loose] {
                let found: Vec<usize> = (0..devices.len())
                    .filter(|&index| matches(devices[index].1))
                    .collect();
                match found[..] {
                    [] => {}
                    [index] => return Ok(index),
                    _ => {
                        return Err(OutputError::AmbiguousDevice {
                            name: name.clone(),
                            ids: found
                                .iter()
                                .map(|&index| devices[index].0.to_owned())
                                .collect(),
                        });
                    }
                }
            }
        }
        Err(OutputError::DeviceNotFound {
            name: wanted.to_owned(),
            available: devices.iter().map(|&(_, name)| name.to_owned()).collect(),
        })
    }
}

/// The stream a session asks a port to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRequest {
    /// Which device; `None` for the system default output.
    pub device: Option<DeviceChoice>,
    /// Frames per second: the song's rate, since nothing resamples.
    pub sample_rate: u32,
    /// Frames per audio callback.
    pub buffer_frames: u32,
    /// Channels the renderer fills in each frame, 1 to
    /// [`domain::MAX_OUTPUTS`]: the stream opens the fewest channels the
    /// device offers at or above this many, plays the renderer's on outputs
    /// 1 to `channels`, and silence on the others. A device with fewer
    /// outputs is refused ([`OutputError::TooFewChannels`]).
    pub channels: u16,
}

/// The stream a port opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Name of the device it plays on.
    pub device: String,
    /// Id of that device ([`DeviceInfo::id`]).
    pub device_id: String,
    /// Frames per second.
    pub sample_rate: u32,
    /// Frames per audio callback.
    pub buffer_frames: u32,
    /// Channels the stream opened on the device, at least
    /// [`OutputRequest::channels`]: the renderer's play on the first ones,
    /// the others get silence.
    pub channels: u16,
}

impl StreamInfo {
    /// Latency one buffer adds: its frames at the sample rate (F8.5).
    pub fn latency(&self) -> Duration {
        Duration::from_secs_f64(f64::from(self.buffer_frames) / f64::from(self.sample_rate))
    }
}

/// Whether an open stream still plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamHealth {
    /// The device calls the renderer.
    Running,
    /// The device stopped calling the renderer for good.
    Lost {
        /// Why, as far as the port knows.
        reason: LossReason,
    },
}

/// Why a stream stopped for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossReason {
    /// The device was unplugged or disappeared.
    Unplugged,
    /// The system invalidated the stream, for example because the device's
    /// sample rate changed.
    StreamInvalidated,
    /// The system moved the stream to another device.
    DeviceChanged,
    /// Another application took the device.
    DeviceBusy,
    /// The audio system itself stopped.
    HostStopped,
    /// The audio system reported another error.
    Failed,
    /// The session closed the stream itself to open it again with new
    /// settings (a buffer size, more outputs for the routing, a setlist
    /// item's sample rate); no port reports it.
    Reconfigured,
}

impl fmt::Display for LossReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unplugged => "the device was unplugged",
            Self::StreamInvalidated => {
                "the system invalidated the stream, for example because the device's sample rate changed"
            }
            Self::DeviceChanged => "the system moved the stream to another device",
            Self::DeviceBusy => "the device is busy with another application",
            Self::HostStopped => "the audio system stopped",
            Self::Failed => "the audio system reported an error",
            Self::Reconfigured => "the stream closed to open again with new settings",
        })
    }
}

/// An open output stream. Dropping it stops the stream and drops the
/// [`AudioRenderer`] it was given.
pub trait OutputStream: Send {
    /// What was opened.
    fn info(&self) -> &StreamInfo;

    /// Whether the stream still plays. Called from the control thread.
    fn health(&self) -> StreamHealth;
}

/// What [`AudioOutput::check`] can tell about a request before any stream
/// opens.
///
/// "Cannot tell" is its own outcome rather than a plain `Ok`: a port that
/// has no check, or a device that cannot be asked right now (busy, or the
/// audio system did not answer), is not the same as a device that was
/// asked and takes the request. Both go ahead ([`Self::goes_ahead`]), and
/// opening decides; only [`Self::Refused`] stops the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputCheck {
    /// The device is there and takes the request, as far as can be told
    /// without a stream: building or starting the stream can still fail.
    Fits,
    /// Opening would refuse the request, for this reason: no such device,
    /// or nothing it supports fits the request.
    Refused(OutputError),
    /// The port cannot tell without opening a stream, for this reason; go
    /// ahead and let opening decide.
    Unknown(String),
}

impl OutputCheck {
    /// Whether the caller should go on and open the stream: unless the
    /// request was refused.
    pub fn goes_ahead(&self) -> bool {
        !matches!(self, Self::Refused(_))
    }
}

/// The audio-output port.
pub trait AudioOutput: Send {
    /// The output devices (F8.1).
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError>;

    /// Checks `request` as [`Self::open`] would before it builds a stream:
    /// that the device it means is there and supports the rate, the buffer
    /// size and the channels. It never opens a stream and never counts as one; a
    /// stream already playing on the device does not count against the
    /// request, since it may be the one about to be replaced. The default,
    /// for ports without a check, is [`OutputCheck::Unknown`].
    fn check(&self, request: &OutputRequest) -> OutputCheck {
        let _ = request;
        OutputCheck::Unknown(
            "this audio output cannot check a request before it opens it".to_owned(),
        )
    }

    /// Opens `request.device` at exactly `request.sample_rate` and
    /// `request.buffer_frames` with at least `request.channels` channels,
    /// plays the renderer's frames of `request.channels` channels on its
    /// first outputs (silence on the others), and starts calling
    /// [`AudioRenderer::render`] from the audio thread.
    ///
    /// The port calls the renderer from one thread at a time, and drops it
    /// when the returned stream is dropped, or before returning an error.
    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: AudioRenderer,
    ) -> Result<Box<dyn OutputStream>, OutputError>;
}

/// Why a port could not list devices or open a stream.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OutputError {
    /// The system has no output device.
    #[error("no audio output device is available")]
    NoDevice,
    /// No device has the requested name or id.
    #[error(
        "no audio output device has the name or id \"{name}\"; available: {}",
        Names(available)
    )]
    DeviceNotFound {
        /// The requested name or id.
        name: String,
        /// Names of the devices there are.
        available: Vec<String>,
    },
    /// More than one device has the requested name.
    #[error(
        "{} audio output devices are named \"{name}\"; choose one by id: {}",
        ids.len(),
        Names(ids)
    )]
    AmbiguousDevice {
        /// The requested name.
        name: String,
        /// Ids of the devices with that name.
        ids: Vec<String>,
    },
    /// The device cannot run at the song's rate (no resampling).
    #[error(
        "audio device \"{device}\" cannot play at {requested} Hz, the song's sample rate (nothing resamples); it supports {}",
        Rates(supported)
    )]
    UnsupportedSampleRate {
        /// Device name.
        device: String,
        /// The song's rate.
        requested: u32,
        /// Inclusive ranges of rates the device supports.
        supported: Vec<(u32, u32)>,
    },
    /// The device cannot use the requested buffer size.
    #[error(
        "audio device \"{device}\" cannot use a buffer of {requested} frames; it supports {min} to {max}"
    )]
    UnsupportedBufferSize {
        /// Device name.
        device: String,
        /// Requested frames.
        requested: u32,
        /// Smallest buffer the device takes.
        min: u32,
        /// Largest buffer the device takes.
        max: u32,
    },
    /// The device has fewer output channels than the request
    /// ([`OutputRequest::channels`]).
    #[error(
        "audio device \"{device}\" has {channels} output channel(s); the routing needs {needed}"
    )]
    TooFewChannels {
        /// Device name.
        device: String,
        /// Its output channels.
        channels: u16,
        /// The channels requested.
        needed: u16,
    },
    /// The device takes no sample format the adapter can write.
    #[error("audio device \"{device}\" takes no sample format this player can write")]
    UnsupportedSampleFormat {
        /// Device name.
        device: String,
    },
    /// The device refused or failed.
    #[error("audio device \"{device}\": {message}")]
    Device {
        /// Device name.
        device: String,
        /// What the system said.
        message: String,
    },
    /// The audio system itself failed, before any device was chosen.
    #[error("audio system: {message}")]
    Host {
        /// What the system said.
        message: String,
    },
}

struct Names<'a>(&'a [String]);

impl fmt::Display for Names<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        for (index, name) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "\"{name}\"")?;
        }
        Ok(())
    }
}

struct Rates<'a>(&'a [(u32, u32)]);

impl fmt::Display for Rates<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("no rate");
        }
        for (index, &(min, max)) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            if min == max {
                write!(f, "{min} Hz")?;
            } else {
                write!(f, "{min} to {max} Hz")?;
            }
        }
        Ok(())
    }
}
