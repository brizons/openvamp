//! An audio output with no hardware, for tests and headless tools.
//!
//! [`FakeOutput`] implements [`AudioOutput`]. Nothing runs on its own: the
//! test calls [`FakeOutput::render`] to run one audio callback, so the
//! player is driven deterministically. Devices can be unplugged and plugged
//! back in, lose their stream for another reason, and change the rates they
//! run at. The n-th device added has the id `fake:n`; the methods that act
//! on one device take its id, since two devices can share a name. Clones
//! share one stream at a time, like an interface that takes one; a check
//! ([`AudioOutput::check`]) looks at the devices only and is counted apart
//! from opens ([`FakeOutput::checks`], [`FakeOutput::open_attempts`]).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::output::{
    AudioOutput, DeviceInfo, LossReason, OutputCheck, OutputError, OutputRequest, OutputStream,
    StreamHealth, StreamInfo,
};
use crate::renderer::AudioRenderer;

/// Name of the device [`FakeOutput::new`] offers.
pub const FAKE_DEVICE: &str = "Fake Output";

/// Id of the device [`FakeOutput::new`] offers.
pub const FAKE_DEVICE_ID: &str = "fake:1";

/// An in-process audio output; clones share the same devices and stream.
#[derive(Clone)]
pub struct FakeOutput {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    audio_thread_drops: AtomicUsize,
}

#[derive(Default)]
struct State {
    devices: Vec<Device>,
    stream: Option<Running>,
    next_id: u64,
    opened: Vec<OutputRequest>,
    open_attempts: usize,
    checks: usize,
    drop_on_audio_thread: bool,
}

struct Device {
    id: String,
    name: String,
    channels: u16,
    /// `None` for any rate.
    rates: Option<Vec<u32>>,
    present: bool,
}

struct Running {
    id: u64,
    renderer: AudioRenderer,
    /// Channels the renderer fills ([`OutputRequest::channels`]).
    channels: usize,
    /// Id of the device it plays on.
    device: String,
    lost: Option<LossReason>,
}

impl Default for FakeOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeOutput {
    /// One stereo device, [`FAKE_DEVICE`], that runs at any rate.
    pub fn new() -> Self {
        Self::without_devices().with_device(FAKE_DEVICE, 2, None)
    }

    /// No device at all.
    pub fn without_devices() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                audio_thread_drops: AtomicUsize::new(0),
            }),
        }
    }

    /// Adds a device with `channels` outputs that runs at `rates` (any rate
    /// when `None`); the n-th device added has the id `fake:n`. The first
    /// device present is the default.
    pub fn with_device(self, name: &str, channels: u16, rates: Option<&[u32]>) -> Self {
        let mut state = self.lock();
        let id = format!("fake:{}", state.devices.len() + 1);
        state.devices.push(Device {
            id,
            name: name.to_owned(),
            channels,
            rates: rates.map(<[u32]>::to_vec),
            present: true,
        });
        drop(state);
        self
    }

    /// Runs one audio callback of `frames` frames and returns its
    /// interleaved output, in the channels the stream was requested with
    /// ([`OutputRequest::channels`]: two for stereo); `None` when no stream
    /// plays.
    pub fn render(&self, frames: usize) -> Option<Vec<f32>> {
        let mut state = self.lock();
        let running = state
            .stream
            .as_mut()
            .filter(|running| running.lost.is_none())?;
        let mut out = vec![0.0f32; running.channels * frames];
        running.renderer.render(&mut out);
        Some(out)
    }

    /// Removes the device with id `id`: a stream on it stops calling back
    /// and reports [`StreamHealth::Lost`] with [`LossReason::Unplugged`].
    pub fn unplug(&self, id: &str) {
        let mut state = self.lock();
        for device in state.devices.iter_mut().filter(|d| d.id == id) {
            device.present = false;
        }
        if let Some(running) = state.stream.as_mut().filter(|r| r.device == id) {
            running.lost = Some(LossReason::Unplugged);
        }
    }

    /// Ends the stream on the device with id `id` for `reason`; the device
    /// stays.
    pub fn lose(&self, id: &str, reason: LossReason) {
        if let Some(running) = self.lock().stream.as_mut().filter(|r| r.device == id) {
            running.lost = Some(reason);
        }
    }

    /// From now on, the device with id `id` runs at `rates` (any rate when
    /// `None`).
    pub fn set_rates(&self, id: &str, rates: Option<&[u32]>) {
        for device in self.lock().devices.iter_mut().filter(|d| d.id == id) {
            device.rates = rates.map(<[u32]>::to_vec);
        }
    }

    /// From now on, the device with id `id` has `channels` outputs, as an
    /// interface set up differently before it is plugged back in.
    pub fn set_channels(&self, id: &str, channels: u16) {
        for device in self.lock().devices.iter_mut().filter(|d| d.id == id) {
            device.channels = channels;
        }
    }

    /// Puts the device with id `id` back.
    pub fn plug(&self, id: &str) {
        for device in self.lock().devices.iter_mut().filter(|d| d.id == id) {
            device.present = true;
        }
    }

    /// Whether a stream is open and playing.
    pub fn is_streaming(&self) -> bool {
        self.lock()
            .stream
            .as_ref()
            .is_some_and(|r| r.lost.is_none())
    }

    /// How many times [`AudioOutput::check`] ran.
    pub fn checks(&self) -> usize {
        self.lock().checks
    }

    /// How many times [`AudioOutput::open`] ran, accepted or refused; a
    /// check never counts.
    pub fn open_attempts(&self) -> usize {
        self.lock().open_attempts
    }

    /// Every request [`AudioOutput::open`] accepted, in order.
    pub fn opened(&self) -> Vec<OutputRequest> {
        self.lock().opened.clone()
    }

    /// From now on, a closed stream drops its renderer on a thread of its
    /// own, as a real audio thread might, without waiting for it.
    pub fn drop_renderers_on_audio_thread(&self) {
        self.lock().drop_on_audio_thread = true;
    }

    /// Renderers that thread has finished dropping.
    pub fn renderers_dropped_on_audio_thread(&self) -> usize {
        self.shared.audio_thread_drops.load(Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl AudioOutput for FakeOutput {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        let state = self.lock();
        let present: Vec<&Device> = state.devices.iter().filter(|d| d.present).collect();
        Ok(present
            .iter()
            .enumerate()
            .map(|(index, device)| DeviceInfo {
                id: device.id.clone(),
                name: device.name.clone(),
                output_channels: device.channels,
                is_default: index == 0,
            })
            .collect())
    }

    /// The device's presence, channels and rates, as [`Self::open`] checks
    /// them; the stream already playing, if any, is left out.
    fn check(&self, request: &OutputRequest) -> OutputCheck {
        let mut state = self.lock();
        state.checks += 1;
        match lookup(&state.devices, request) {
            Ok(_) => OutputCheck::Fits,
            Err(error) => OutputCheck::Refused(error),
        }
    }

    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: AudioRenderer,
    ) -> Result<Box<dyn OutputStream>, OutputError> {
        let mut state = self.lock();
        state.open_attempts += 1;
        let device = lookup(&state.devices, request)?;
        if state.stream.is_some() {
            return Err(OutputError::Device {
                device: device.name.clone(),
                message: "the fake output plays one stream at a time".to_owned(),
            });
        }
        let info = StreamInfo {
            device: device.name.clone(),
            device_id: device.id.clone(),
            sample_rate: request.sample_rate,
            buffer_frames: request.buffer_frames,
            // It opens exactly the channels requested.
            channels: request.channels,
        };
        state.next_id += 1;
        let id = state.next_id;
        state.opened.push(request.clone());
        state.stream = Some(Running {
            id,
            renderer,
            channels: usize::from(request.channels),
            device: info.device_id.clone(),
            lost: None,
        });
        Ok(Box::new(FakeStream {
            shared: Arc::clone(&self.shared),
            id,
            info,
        }))
    }
}

/// The present device `request` means, if it has the request's channels and
/// plays at its rate: what [`FakeOutput`] checks before it opens a stream.
fn lookup<'a>(devices: &'a [Device], request: &OutputRequest) -> Result<&'a Device, OutputError> {
    let present: Vec<&Device> = devices.iter().filter(|d| d.present).collect();
    let device = match &request.device {
        Some(choice) => {
            let keys: Vec<(&str, &str)> = present
                .iter()
                .map(|d| (d.id.as_str(), d.name.as_str()))
                .collect();
            present[choice.find(&keys)?]
        }
        None => present.first().ok_or(OutputError::NoDevice)?,
    };
    if device.channels < request.channels {
        return Err(OutputError::TooFewChannels {
            device: device.name.clone(),
            channels: device.channels,
            needed: request.channels,
        });
    }
    if let Some(rates) = &device.rates
        && !rates.contains(&request.sample_rate)
    {
        return Err(OutputError::UnsupportedSampleRate {
            device: device.name.clone(),
            requested: request.sample_rate,
            supported: rates.iter().map(|&rate| (rate, rate)).collect(),
        });
    }
    Ok(device)
}

struct FakeStream {
    shared: Arc<Shared>,
    id: u64,
    info: StreamInfo,
}

impl FakeStream {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl OutputStream for FakeStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn health(&self) -> StreamHealth {
        match self.lock().stream.as_ref().filter(|r| r.id == self.id) {
            Some(Running { lost: None, .. }) => StreamHealth::Running,
            Some(Running {
                lost: Some(reason), ..
            }) => StreamHealth::Lost { reason: *reason },
            None => StreamHealth::Lost {
                reason: LossReason::Unplugged,
            },
        }
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        let (running, on_audio_thread) = {
            let mut state = self.lock();
            let mine = state.stream.as_ref().is_some_and(|r| r.id == self.id);
            (
                if mine { state.stream.take() } else { None },
                state.drop_on_audio_thread,
            )
        };
        let Some(running) = running else {
            return;
        };
        if on_audio_thread {
            let shared = Arc::clone(&self.shared);
            thread::Builder::new()
                .name("fake-audio".to_owned())
                .spawn(move || {
                    drop(running);
                    shared.audio_thread_drops.fetch_add(1, Ordering::SeqCst);
                })
                .expect("the fake audio thread starts");
        } else {
            drop(running);
        }
    }
}
