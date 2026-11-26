//! Audio output through `cpal` (S4): the [`app::AudioOutput`] port on real
//! devices.
//!
//! [`CpalOutput`] lists the output devices with their channel counts (F8.1)
//! and opens one stream per session: at the song's sample rate (a device that
//! cannot run at it is an error; nothing resamples), with a fixed buffer of
//! the size the session asks for (F8.5), and the fewest channels the device
//! offers at or above the ones the session renders (its routing's outputs,
//! F8.2), which play on the device's first outputs; any other output gets
//! silence. A session that asks for the default output gets the device that
//! is the default when it opens, pinned by its id, so the stream never moves
//! to another device on its own.
//!
//! Opening and [checking](app::AudioOutput::check) a request take the same
//! steps before a stream is built ([`plan`]): find the device, read the
//! configurations it supports, pick one. So a check refuses exactly what
//! opening would refuse at that point, and never builds a stream. What
//! cannot be read (the device list, or the configurations of a device that
//! is busy, which on ALSA are read by opening its PCM for a moment) makes a
//! check [`OutputCheck::Unknown`]: the caller goes ahead and opening
//! decides.
//!
//! # Device identity
//!
//! A device's id ([`app::DeviceInfo::id`]) is `cpal`'s `DeviceId`, printed
//! as `host:device`: the device UID on macOS (Core Audio), the endpoint id
//! on Windows (WASAPI), the PCM name on Linux (ALSA). Core Audio and WASAPI
//! give each device its own id, kept across unplugging and reboots, so two
//! identical interfaces never share one and a lost device is found again
//! only when that very device is back. ALSA names a card by the index or id
//! the kernel gives it in plug order (`hw:CARD=Interface,DEV=0`, then
//! `Interface_1` for a second identical card): two identical interfaces
//! still have different ids, but they can swap them when both are plugged
//! back in the other order. The system default output is pinned by its id
//! too; when it is not among the listed devices, it is offered on its own,
//! so a session can come back to it.
//!
//! Devices are read without `cpal`'s `Display`, which fails while a device
//! disappears: a device whose id cannot be read is left out (it cannot be
//! pinned, and it is going away), one whose name cannot be read is listed
//! as [`UNNAMED`].
//!
//! The data callback obeys the real-time checklist: the scratch buffer the
//! renderer fills is allocated when the stream is built, and the callback only renders into
//! it and converts samples; no allocation, lock, I/O or logging. The error
//! callback, which some systems call on the audio thread, only stores an
//! atomic. A stream reports [`StreamHealth::Lost`] once its device is gone
//! or the system invalidated it; the session then pauses and reopens the
//! device when it is back.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use app::{
    AudioOutput, AudioRenderer, DeviceChoice, DeviceInfo, LossReason, OutputCheck, OutputError,
    OutputRequest, OutputStream, StreamHealth, StreamInfo,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, ErrorKind, FromSample, Sample, SampleFormat, SizedSample, SupportedBufferSize,
    SupportedStreamConfigRange,
};

/// The [`AudioOutput`] port on the system's default audio host.
pub struct CpalOutput {
    host: cpal::Host,
}

impl Default for CpalOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl CpalOutput {
    /// Uses the system's default audio host.
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
        }
    }

    /// The devices a session can choose, with their `cpal` devices.
    fn scan(&self) -> Result<Vec<(Candidate, cpal::Device)>, OutputError> {
        let listed = self
            .host
            .output_devices()
            .map_err(|error| OutputError::Host {
                message: error.to_string(),
            })?;
        let default = self
            .host
            .default_output_device()
            .map(|device| (probe(&device), device));
        Ok(candidates(
            listed.map(|device| (probe(&device), device)),
            default,
        ))
    }
}

/// Reads a device's id and name without its `Display`, which fails while
/// the device disappears.
fn probe(device: &cpal::Device) -> Probe {
    Probe {
        id: device.id().ok().map(|id| id.to_string()),
        name: device
            .description()
            .ok()
            .map(|description| description.name().to_owned()),
    }
}

impl AudioOutput for CpalOutput {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        Ok(self
            .scan()?
            .into_iter()
            .map(|(candidate, device)| DeviceInfo {
                output_channels: device
                    .supported_output_configs()
                    .map(|ranges| ranges.map(|range| range.channels()).max().unwrap_or(0))
                    .unwrap_or(0),
                id: candidate.id,
                name: candidate.name,
                is_default: candidate.is_default,
            })
            .collect())
    }

    /// The steps opening takes before it builds a stream ([`plan`]): the
    /// device, its configurations and the one to open. A device list or
    /// configurations that cannot be read, such as a busy ALSA device's,
    /// make it [`OutputCheck::Unknown`], never a refusal.
    fn check(&self, request: &OutputRequest) -> OutputCheck {
        check_outcome(&plan(self.scan(), request, read_configs))
    }

    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: AudioRenderer,
    ) -> Result<Box<dyn OutputStream>, OutputError> {
        let Plan {
            candidate: Candidate { id, name, .. },
            device,
            choice,
        } = open_outcome(plan(self.scan(), request, read_configs))?;
        let device_error = |error: cpal::Error| OutputError::Device {
            device: name.clone(),
            message: error.to_string(),
        };
        let config = cpal::StreamConfig {
            channels: choice.channels,
            sample_rate: request.sample_rate,
            buffer_size: BufferSize::Fixed(request.buffer_frames),
        };
        let health = Arc::new(AtomicU8::new(RUNNING));
        let scratch = Scratch {
            channels: usize::from(request.channels),
            frames: request.buffer_frames as usize,
        };
        let built = match choice.format {
            SampleFormat::F32 => build::<f32>(&device, &config, renderer, &health, scratch),
            SampleFormat::I32 => build::<i32>(&device, &config, renderer, &health, scratch),
            SampleFormat::I16 => build::<i16>(&device, &config, renderer, &health, scratch),
            SampleFormat::F64 => build::<f64>(&device, &config, renderer, &health, scratch),
            SampleFormat::U16 => build::<u16>(&device, &config, renderer, &health, scratch),
            _ => unreachable!("`choose_config` picks only formats `rank` knows"),
        };
        let stream = built.map_err(device_error)?;
        stream.play().map_err(device_error)?;
        let buffer_frames = stream.buffer_size().unwrap_or(request.buffer_frames);
        Ok(Box::new(CpalStream {
            _stream: stream,
            info: StreamInfo {
                device: name.clone(),
                device_id: id,
                sample_rate: request.sample_rate,
                buffer_frames,
                channels: choice.channels,
            },
            health,
        }))
    }
}

/// The buffer the renderer fills: frames of the request's channels, as many
/// as one callback holds.
#[derive(Debug, Clone, Copy)]
struct Scratch {
    channels: usize,
    frames: usize,
}

/// Builds a stream of samples of type `T` that renders through `renderer`.
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut renderer: AudioRenderer,
    health: &Arc<AtomicU8>,
    scratch: Scratch,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = usize::from(config.channels);
    // Allocated here, never in the callback.
    let mut rendered = vec![0.0f32; scratch.channels * scratch.frames];
    let health = Arc::clone(health);
    device.build_output_stream::<T, _, _>(
        *config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            write_interleaved(data, channels, &mut rendered, scratch.channels, |piece| {
                renderer.render(piece);
            });
        },
        move |error: cpal::Error| {
            if let Some(reason) = loss_reason(error.kind()) {
                health.store(encode(reason), Ordering::SeqCst);
            }
        },
        None,
    )
}

/// Name listed for a device whose name cannot be read.
pub const UNNAMED: &str = "(unnamed device)";

/// What `cpal` said about one output device: its id and its name, each
/// `None` when it could not be read, for example while the device
/// disappears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The device's `DeviceId`, printed as `host:device`.
    pub id: Option<String>,
    /// The device's name.
    pub name: Option<String>,
}

/// An output device a session can choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Its id ([`app::DeviceInfo::id`]).
    pub id: String,
    /// Its name, or [`UNNAMED`].
    pub name: String,
    /// Whether it is the system's default output.
    pub is_default: bool,
}

/// The devices a session can choose, each with the `T` the caller attached
/// to its probe (the `cpal` device): the `listed` output devices in order,
/// the one with the `default` device's id marked as the default, and the
/// default device itself at the end when no listed device has its id. A
/// device whose id cannot be read is left out, as is a second device with
/// an id already taken; one whose name cannot be read is named [`UNNAMED`].
pub fn candidates<T>(
    listed: impl IntoIterator<Item = (Probe, T)>,
    default: Option<(Probe, T)>,
) -> Vec<(Candidate, T)> {
    let candidate = |id, name: Option<String>, is_default| Candidate {
        id,
        name: name.unwrap_or_else(|| UNNAMED.to_owned()),
        is_default,
    };
    let mut found: Vec<(Candidate, T)> = Vec::new();
    for (Probe { id, name }, device) in listed {
        let Some(id) = id else { continue };
        if found.iter().all(|(taken, _)| taken.id != id) {
            found.push((candidate(id, name, false), device));
        }
    }
    if let Some((Probe { id: Some(id), name }, device)) = default {
        match found.iter_mut().find(|(listed, _)| listed.id == id) {
            Some((listed, _)) => listed.is_default = true,
            None => found.push((candidate(id, name, true), device)),
        }
    }
    found
}

/// The index of the device `choice` means among `candidates`; the default
/// output for `None`.
pub fn select<T>(
    candidates: &[(Candidate, T)],
    choice: Option<&DeviceChoice>,
) -> Result<usize, OutputError> {
    match choice {
        None => candidates
            .iter()
            .position(|(candidate, _)| candidate.is_default)
            .ok_or(OutputError::NoDevice),
        Some(choice) => {
            let keys: Vec<(&str, &str)> = candidates
                .iter()
                .map(|(candidate, _)| (candidate.id.as_str(), candidate.name.as_str()))
                .collect();
            choice.find(&keys)
        }
    }
}

/// The device and the configuration [`CpalOutput`] builds a stream with.
#[derive(Debug)]
pub struct Plan<T> {
    /// The device the request means.
    pub candidate: Candidate,
    /// What the caller attached to it: the `cpal` device.
    pub device: T,
    /// The configuration to open.
    pub choice: StreamChoice,
}

/// Why [`plan`] found nothing to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unplanned {
    /// Opening refuses the request: no device it means, or nothing the
    /// device supports fits it. A check refuses it too.
    Refused(OutputError),
    /// The device list or the device's configurations could not be read
    /// (the audio system did not answer, or the device was busy: on ALSA,
    /// reading them opens the PCM for a moment). Opening fails with this
    /// error; a check cannot tell, and lets opening decide.
    Unreadable(OutputError),
}

/// What [`CpalOutput`] does before it builds a stream, for both opening
/// and checking: the device `request` means among `found` (the device scan,
/// or why it failed; see [`select`]), the configurations it supports, which
/// `configs` reads only for that device, and the one to open
/// ([`choose_config`]). It never builds a stream.
pub fn plan<T>(
    found: Result<Vec<(Candidate, T)>, OutputError>,
    request: &OutputRequest,
    configs: impl FnOnce(&T) -> Result<Vec<SupportedStreamConfigRange>, String>,
) -> Result<Plan<T>, Unplanned> {
    let mut found = found.map_err(Unplanned::Unreadable)?;
    let index = select(&found, request.device.as_ref()).map_err(Unplanned::Refused)?;
    let (candidate, device) = found.swap_remove(index);
    let ranges = configs(&device).map_err(|message| {
        Unplanned::Unreadable(OutputError::Device {
            device: candidate.name.clone(),
            message,
        })
    })?;
    let choice = choose_config(
        &candidate.name,
        &ranges,
        request.sample_rate,
        request.buffer_frames,
        request.channels,
    )
    .map_err(Unplanned::Refused)?;
    Ok(Plan {
        candidate,
        device,
        choice,
    })
}

/// What a check says about `plan`: it fits, it is refused for the reason
/// opening would give, or, when something could not be read, it cannot
/// tell.
pub fn check_outcome<T>(plan: &Result<Plan<T>, Unplanned>) -> OutputCheck {
    match plan {
        Ok(_) => OutputCheck::Fits,
        Err(Unplanned::Refused(error)) => OutputCheck::Refused(error.clone()),
        Err(Unplanned::Unreadable(error)) => OutputCheck::Unknown(error.to_string()),
    }
}

/// What opening does with `plan`: builds from it, or fails with its error.
pub fn open_outcome<T>(plan: Result<Plan<T>, Unplanned>) -> Result<Plan<T>, OutputError> {
    plan.map_err(|unplanned| match unplanned {
        Unplanned::Refused(error) | Unplanned::Unreadable(error) => error,
    })
}

/// Reads the configurations `device` supports; on ALSA this opens its PCM
/// for a moment.
fn read_configs(device: &cpal::Device) -> Result<Vec<SupportedStreamConfigRange>, String> {
    device
        .supported_output_configs()
        .map(Iterator::collect)
        .map_err(|error| error.to_string())
}

/// A playing `cpal` stream; dropping it stops the stream and drops the
/// renderer, which hands the player back to the session.
struct CpalStream {
    _stream: cpal::Stream,
    info: StreamInfo,
    health: Arc<AtomicU8>,
}

impl OutputStream for CpalStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn health(&self) -> StreamHealth {
        match decode(self.health.load(Ordering::SeqCst)) {
            None => StreamHealth::Running,
            Some(reason) => StreamHealth::Lost { reason },
        }
    }
}

/// The stream configuration [`choose_config`] picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamChoice {
    /// Channels to open; the renderer's play on the first
    /// [`app::OutputRequest::channels`].
    pub channels: u16,
    /// Sample format to write.
    pub format: SampleFormat,
}

/// Picks, among the configurations a device supports (`ranges`), one with at
/// least `channels` channels (one at least) that runs at `sample_rate` with
/// a buffer of `frames` frames: a float format first, then the fewest
/// channels. Says why there is none: a device with fewer outputs, without
/// the song's rate (with the rates it has), without the buffer size (with
/// the sizes it takes), or without a format this adapter writes.
pub fn choose_config(
    device: &str,
    ranges: &[SupportedStreamConfigRange],
    sample_rate: u32,
    frames: u32,
    channels: u16,
) -> Result<StreamChoice, OutputError> {
    let wide: Vec<&SupportedStreamConfigRange> = ranges
        .iter()
        .filter(|range| range.channels() >= channels.max(1))
        .collect();
    if wide.is_empty() {
        return Err(OutputError::TooFewChannels {
            device: device.to_owned(),
            channels: ranges
                .iter()
                .map(|range| range.channels())
                .max()
                .unwrap_or(0),
            needed: channels,
        });
    }
    let at_rate: Vec<&SupportedStreamConfigRange> = wide
        .iter()
        .copied()
        .filter(|range| {
            range.min_sample_rate() <= sample_rate && sample_rate <= range.max_sample_rate()
        })
        .collect();
    if at_rate.is_empty() {
        let mut supported: Vec<(u32, u32)> = wide
            .iter()
            .map(|range| (range.min_sample_rate(), range.max_sample_rate()))
            .collect();
        supported.sort_unstable();
        supported.dedup();
        return Err(OutputError::UnsupportedSampleRate {
            device: device.to_owned(),
            requested: sample_rate,
            supported,
        });
    }
    let writable: Vec<&SupportedStreamConfigRange> = at_rate
        .into_iter()
        .filter(|range| rank(range.sample_format()).is_some())
        .collect();
    if writable.is_empty() {
        return Err(OutputError::UnsupportedSampleFormat {
            device: device.to_owned(),
        });
    }
    let fits = |range: &SupportedStreamConfigRange| match *range.buffer_size() {
        SupportedBufferSize::Range { min, max } => min <= frames && frames <= max,
        SupportedBufferSize::Unknown => true,
    };
    let Some(best) = writable
        .iter()
        .copied()
        .filter(|range| fits(range))
        .min_by_key(|range| (rank(range.sample_format()), range.channels()))
    else {
        let bounds = writable
            .iter()
            .filter_map(|range| match *range.buffer_size() {
                SupportedBufferSize::Range { min, max } => Some((min, max)),
                SupportedBufferSize::Unknown => None,
            });
        let (min, max) = bounds.fold((u32::MAX, 0), |(lo, hi), (min, max)| {
            (lo.min(min), hi.max(max))
        });
        return Err(OutputError::UnsupportedBufferSize {
            device: device.to_owned(),
            requested: frames,
            min,
            max,
        });
    };
    Ok(StreamChoice {
        channels: best.channels(),
        format: best.sample_format(),
    })
}

/// Preference among the formats this adapter writes; `None` for the others.
fn rank(format: SampleFormat) -> Option<u8> {
    match format {
        SampleFormat::F32 => Some(0),
        SampleFormat::F64 => Some(1),
        SampleFormat::I32 => Some(2),
        SampleFormat::I16 => Some(3),
        SampleFormat::U16 => Some(4),
        _ => None,
    }
}

/// Fills `data`, interleaved frames of `channels` samples, from frames of
/// `rendered_channels` samples the renderer writes into `rendered` in
/// pieces of at most `rendered.len() / rendered_channels` frames: rendered
/// channel n on output n, silence on the outputs past them. A device with
/// fewer channels than the renderer, which [`choose_config`] never picks,
/// gets silence. Real-time safe: it only uses the buffers it is given.
pub fn write_interleaved<T>(
    data: &mut [T],
    channels: usize,
    rendered: &mut [f32],
    rendered_channels: usize,
    mut render: impl FnMut(&mut [f32]),
) where
    T: Sample + FromSample<f32>,
{
    let piece_frames = rendered.len().checked_div(rendered_channels).unwrap_or(0);
    if rendered_channels == 0 || channels < rendered_channels || piece_frames == 0 {
        data.fill(T::EQUILIBRIUM);
        return;
    }
    let whole = data.len() - data.len() % channels;
    let (frames, rest) = data.split_at_mut(whole);
    rest.fill(T::EQUILIBRIUM);
    for chunk in frames.chunks_mut(piece_frames * channels) {
        let piece = &mut rendered[..rendered_channels * (chunk.len() / channels)];
        render(piece);
        for (frame, heard) in chunk
            .chunks_exact_mut(channels)
            .zip(piece.chunks_exact(rendered_channels))
        {
            let (played, silent) = frame.split_at_mut(rendered_channels);
            for (sample, &value) in played.iter_mut().zip(heard) {
                *sample = T::from_sample(value);
            }
            silent.fill(T::EQUILIBRIUM);
        }
    }
}

/// A stream that still plays.
const RUNNING: u8 = 0;

/// Every loss reason, in the order the health atomic encodes them from 1.
const LOSSES: [LossReason; 6] = [
    LossReason::Unplugged,
    LossReason::StreamInvalidated,
    LossReason::DeviceChanged,
    LossReason::DeviceBusy,
    LossReason::HostStopped,
    LossReason::Failed,
];

/// What a stream error means for the session: why the stream is lost, or
/// `None` when it plays on (an xrun, or real-time priority refused).
pub fn loss_reason(kind: ErrorKind) -> Option<LossReason> {
    match kind {
        ErrorKind::Xrun | ErrorKind::RealtimeDenied => None,
        ErrorKind::DeviceNotAvailable => Some(LossReason::Unplugged),
        ErrorKind::StreamInvalidated => Some(LossReason::StreamInvalidated),
        ErrorKind::DeviceChanged => Some(LossReason::DeviceChanged),
        ErrorKind::DeviceBusy => Some(LossReason::DeviceBusy),
        ErrorKind::HostUnavailable => Some(LossReason::HostStopped),
        _ => Some(LossReason::Failed),
    }
}

/// The health atomic's value for a stream lost for `reason`.
fn encode(reason: LossReason) -> u8 {
    let index = LOSSES
        .iter()
        .position(|&loss| loss == reason)
        .expect("`LOSSES` lists every reason");
    index as u8 + 1
}

/// The loss reason the health atomic holds; `None` while the stream plays.
fn decode(code: u8) -> Option<LossReason> {
    code.checked_sub(1)
        .and_then(|index| LOSSES.get(usize::from(index)).copied())
}
