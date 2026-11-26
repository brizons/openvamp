//! The Audio output screen without a window (S3, F8): the output devices
//! with their output counts and whether the one in use plays, the rate the
//! device runs at against the song's, the buffer size with its latency,
//! split mode for 2-output devices, and the routing matrix of buses (with
//! the Mixer's bus icons) by outputs with its presets. Every change
//! applies to the song playing at once and is saved for its device (keyed
//! by the device's id); with nothing playing, the chosen device's routing
//! is edited and saved for the next session. A change that needs the
//! stream to open again is saved once the stream plays with it, and
//! otherwise says why it is not ([`OutputPage::follow`]).

use std::fmt;
use std::time::{Duration, Instant};

use app::{
    DeviceInfo, DeviceStatus, LossReason, MAX_BUFFER_FRAMES, MIN_BUFFER_FRAMES, OutputError,
    RuntimeError, StreamInfo,
};
use domain::{Bus, Destination, Routing};

use crate::audio_settings::AudioSettings;
use crate::mixer::bus_icon;
use crate::show::Show;

/// The buffer sizes offered, in frames (F8.5).
pub const BUFFER_SIZES: [u32; 5] = [64, 128, 256, 512, 1024];

/// How long a change waits for the device to open with it, at most, when
/// nothing else settles it: longer than the session waits for a playing
/// stream's ramp out before it opens the stream again (2 s), and than it
/// waits between tries of a device it holds (0.5 s).
pub const APPLY_PATIENCE: Duration = Duration::from_secs(3);

/// The output counts with a preset of their own in the Preset menu.
const PRESETS: [u16; 6] = [1, 2, 3, 4, 6, 8];

/// What a click on output `output` of bus `bus`'s row makes of `routing`.
/// A bus on one output gets a stereo pair with the output clicked; a bus on
/// a pair keeps the other side alone when one side is clicked, and a
/// third output starts over: the bus plays on that output alone. The only
/// output of a bus stays, since a bus always plays somewhere. In split
/// mode a click turns split mode off and edits the buses as stored.
pub fn click_cell(routing: &Routing, bus: Bus, output: u16) -> Routing {
    let mut routing = routing.clone();
    routing.split_mode = false;
    let destination = match routing.destination(bus) {
        Destination::Mono(alone) if alone == output => Destination::Mono(alone),
        Destination::Mono(alone) => Destination::Stereo {
            left: alone.min(output),
            right: alone.max(output),
        },
        Destination::Stereo { left, right } if output == left => Destination::Mono(right),
        Destination::Stereo { left, right } if output == right => Destination::Mono(left),
        Destination::Stereo { .. } => Destination::Mono(output),
    };
    routing.buses.insert(bus, destination);
    routing
}

/// A problem the session reports that the screen shows:
/// [`RuntimeError::RoutingRefitted`].
pub fn problem_notice(error: &RuntimeError) -> Option<String> {
    matches!(error, RuntimeError::RoutingRefitted { .. }).then(|| error.to_string())
}

/// Why a device could not be opened, when the screen shows it:
/// [`OutputError::TooFewChannels`].
pub fn output_notice(error: &OutputError) -> Option<String> {
    matches!(error, OutputError::TooFewChannels { .. }).then(|| error.to_string())
}

/// The refusal in the session's answer to a buffer size or a routing:
/// any error but a full queue ([`RuntimeError::QueueFull`]), after which
/// the session still takes the change from its next polls.
pub fn refusal(answer: Result<(), RuntimeError>) -> Option<RuntimeError> {
    match answer {
        Ok(()) | Err(RuntimeError::QueueFull { .. }) => None,
        Err(error) => Some(error),
    }
}

/// What the screen says of a buffer size it does not keep, and why.
fn buffer_not_kept(why: impl fmt::Display) -> String {
    format!("The buffer size could not be changed: {why}")
}

/// What the screen says of a routing it does not keep, and why.
fn routing_not_kept(why: impl fmt::Display) -> String {
    format!("The routing could not be applied: {why}")
}

/// Saves `routing` for the device with id `device`; what the screen says
/// when it could not.
fn save_routing(settings: &mut AudioSettings, device: &str, routing: &Routing) -> Option<String> {
    settings
        .save_routing(device, routing)
        .err()
        .map(|error| format!("The routing could not be saved: {error}"))
}

/// Why the session holds the device `device`, lost for `reason`, instead
/// of playing on it: why it last refused to open again, else how it was
/// lost.
fn held_why(device: &str, reason: LossReason, reopen_error: Option<&OutputError>) -> String {
    match (reopen_error, reason) {
        (Some(error), _) => error.to_string(),
        (None, LossReason::Reconfigured) => format!("{device} refuses the new settings"),
        (None, reason) => RuntimeError::DeviceLost {
            device: device.to_owned(),
            reason,
        }
        .to_string(),
    }
}

/// The name of the device the session plays on, or holds.
fn device_name(status: &DeviceStatus) -> &str {
    match status {
        DeviceStatus::Connected(stream) => &stream.device,
        DeviceStatus::Disconnected { device, .. } | DeviceStatus::PlayerLost { device } => device,
    }
}

/// The stream the session plays on, when it plays on one.
fn stream(show: &Show) -> Option<&StreamInfo> {
    match &show.snapshot().device {
        DeviceStatus::Connected(stream) => Some(stream),
        _ => None,
    }
}

/// The outputs the stream must have for `routing` on a device with
/// `outputs` outputs: the highest one the fitted routing uses, at least
/// two (as the session opens it).
fn channels_for(routing: &Routing, outputs: u16) -> u16 {
    let fitted = routing.fit(outputs);
    let highest = Bus::ALL
        .iter()
        .map(|&bus| fitted.destination(bus))
        .chain(fitted.master_destination())
        .flat_map(Destination::outputs)
        .max()
        .unwrap_or(1);
    highest.max(fitted.outputs.min(2))
}

/// How a change waiting for the stream to open again ends.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// A stream plays with it: it is kept (saved).
    Kept,
    /// It is not kept, for this reason.
    NotKept(String),
}

/// What the session's device, `status`, says of a change made while
/// `before` played (`None`: the device was held already), once it has
/// `waited` [`APPLY_PATIENCE`] or not; `takes` is whether a stream plays
/// with the change, `misses` what one plays instead. `None` while the
/// change still waits:
/// - a stream that plays with it keeps it;
/// - a stream opened since without it (the device rounded or clamped it)
///   ends it, and so does the stream of before once the patience is over,
///   since until then it may still be ramping out;
/// - a hold that came after the change ends it, saying why: the device
///   refused to open with it ([`LossReason::Reconfigured`]) or was lost
///   first. A hold already there is no refusal of this change, which the
///   session tries the device with meanwhile: it ends it only once the
///   patience is over;
/// - a player lost for good ends it at once.
fn verdict(
    status: &DeviceStatus,
    before: Option<&StreamInfo>,
    waited: bool,
    takes: impl Fn(&StreamInfo) -> bool,
    misses: impl Fn(&StreamInfo) -> String,
) -> Option<Verdict> {
    match status {
        DeviceStatus::Connected(stream) if takes(stream) => Some(Verdict::Kept),
        DeviceStatus::Connected(stream) => {
            (waited || before != Some(stream)).then(|| Verdict::NotKept(misses(stream)))
        }
        DeviceStatus::Disconnected {
            device,
            reason,
            reopen_error,
        } => (waited || before.is_some())
            .then(|| Verdict::NotKept(held_why(device, *reason, reopen_error.as_ref()))),
        DeviceStatus::PlayerLost { .. } => {
            Some(Verdict::NotKept(RuntimeError::PlayerLost.to_string()))
        }
    }
}

/// Whether a change made at `since` has waited [`APPLY_PATIENCE`] at `now`.
fn waited(since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(since) >= APPLY_PATIENCE
}

/// A buffer size the session took that waits for the stream to open again
/// with it ([`verdict`]). It may be asked for while the device is held:
/// the session tries the device with it, which may refuse the size.
#[derive(Debug)]
struct PendingBuffer {
    frames: u32,
    /// When it was asked for.
    since: Instant,
    /// The device's name, as the session gave it then.
    device: String,
    /// The stream that played then; `None` when the device was held
    /// already, after an earlier refusal or a loss, which is then not this
    /// change's.
    before: Option<StreamInfo>,
}

impl PendingBuffer {
    fn verdict(&self, status: &DeviceStatus, now: Instant) -> Option<Verdict> {
        verdict(
            status,
            self.before.as_ref(),
            waited(self.since, now),
            |stream| stream.buffer_frames == self.frames,
            |stream| {
                format!(
                    "audio device \"{}\" plays buffers of {} frames, not {}",
                    stream.device, stream.buffer_frames, self.frames
                )
            },
        )
    }
}

/// A routing the session took that needs more outputs than the stream
/// has, waiting for the stream to open again with them ([`verdict`]).
/// Unlike a buffer size, it never waits on a device held before it, so
/// `before` is always a stream: a routing given to a held device is saved
/// at once ([`OutputPage::apply`]), since the session opens the device
/// with it fitted to the outputs the device has, which no device refuses.
/// A hold that comes after it is the refusal of the stream it needs, even
/// when the device refused a buffer size asked for meanwhile: neither
/// plays.
#[derive(Debug)]
struct PendingRouting {
    /// The id of the device it is saved for.
    device: String,
    routing: Routing,
    /// When it was given.
    since: Instant,
    /// The stream that played then.
    before: StreamInfo,
}

impl PendingRouting {
    /// [`verdict`], with `outputs` the outputs the session now knows the
    /// device has: one that came back with fewer plays the routing fitted
    /// to them, which the session took.
    fn verdict(&self, status: &DeviceStatus, outputs: u16, now: Instant) -> Option<Verdict> {
        let needed = channels_for(&self.routing, outputs);
        verdict(
            status,
            Some(&self.before),
            waited(self.since, now),
            |stream| stream.channels >= needed,
            |stream| {
                format!(
                    "audio device \"{}\" plays {} outputs, not the {needed} the routing needs",
                    stream.device, stream.channels
                )
            },
        )
    }
}

/// The preset `routing` is for a device with `outputs` outputs, such as
/// "8 outputs" (the device's own first, then the Preset menu's), or
/// "Custom" once it is none of them.
fn preset_name(routing: &Routing, outputs: u16) -> String {
    std::iter::once(outputs)
        .chain(PRESETS)
        .find(|&preset| Routing::preset(preset).fit(outputs) == *routing)
        .map_or_else(|| "Custom".to_owned(), outputs_text)
}

/// "8 kHz", "44.1 kHz".
fn rate_text(rate: u32) -> String {
    if rate.is_multiple_of(1_000) {
        format!("{} kHz", rate / 1_000)
    } else {
        format!("{:.1} kHz", f64::from(rate) / 1_000.0)
    }
}

fn outputs_text(outputs: u16) -> String {
    format!("{outputs} output{}", if outputs == 1 { "" } else { "s" })
}

/// What the screen says after the rate a device runs at, `device`, of the
/// song's rate `song`: "matches the song", or the song's rate when they
/// differ (nothing resamples, so the session plays at the song's rate).
pub fn rate_note(device: u32, song: u32) -> String {
    if device == song {
        "matches the song".to_owned()
    } else {
        format!("the song is {}", rate_text(song))
    }
}

/// What the dot on the device button says of the device in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// No song is open, so no stream: the screen claims nothing (no dot).
    Closed,
    /// A stream is open on the device.
    Open,
    /// The session lost the device, or it refused to open again; the
    /// session keeps trying.
    Lost,
}

/// One device of the device menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Such as "Interface · 8 outputs".
    pub label: String,
    /// Whether it is the device in use (or chosen, with nothing playing).
    pub chosen: bool,
}

/// One bus's row of the routing matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRow {
    /// The bus.
    pub bus: Bus,
    /// Its name.
    pub name: &'static str,
    /// Its icon, as the Mixer's bus strip draws it ([`bus_icon`]).
    pub icon: &'static str,
    /// Whether it plays on each output, from output 1.
    pub cells: Vec<bool>,
}

/// Everything the Audio output screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputScreen {
    /// The devices, in the order the system lists them.
    pub devices: Vec<DeviceRow>,
    /// The device in use, as its row reads; "No output device" without one.
    pub device: String,
    /// The dot on the device button.
    pub device_state: DeviceState,
    /// Whether the device menu is open.
    pub device_menu: bool,
    /// The rate the device runs at, such as "48 kHz"; "Set by the song
    /// that plays" with nothing playing.
    pub sample_rate: String,
    /// What follows the rate while a stream plays ([`rate_note`]); `None`
    /// with nothing playing.
    pub sample_rate_note: Option<String>,
    /// The buffer sizes offered.
    pub buffer_sizes: [u32; 5],
    /// The buffer size in use.
    pub buffer: u32,
    /// The latency the buffer adds, such as "5.3 ms"; empty with nothing
    /// playing, since it depends on the rate.
    pub latency: String,
    /// Whether split mode is on, for a 2-output device; `None` for others.
    pub split_mode: Option<bool>,
    /// The line beside the matrix's title.
    pub hint: &'static str,
    /// The device's outputs: the matrix's columns.
    pub outputs: u16,
    /// The matrix, a row per bus in mixer order.
    pub rows: Vec<RoutingRow>,
    /// Such as "Preset: 8 outputs".
    pub preset: String,
    /// Whether the Preset menu is open.
    pub preset_menu: bool,
    /// The presets the device can play, as (outputs, label).
    pub presets: Vec<(u16, String)>,
    /// Whether a change waits for the device to open again with it.
    pub applying: bool,
    /// Why a settings file is not used, a routing was refitted or refused,
    /// a device has too few outputs, or a change could not be saved.
    pub notices: Vec<String>,
}

/// What an Audio output screen control asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputAction {
    /// The device button: shows the devices, or hides them.
    OpenDeviceMenu,
    /// Plays on the device at this index of the menu.
    ChooseDevice(usize),
    /// A buffer size.
    Buffer(u32),
    /// The split mode switch.
    ToggleSplit,
    /// A cell of the matrix ([`click_cell`]).
    Cell {
        /// Its row.
        bus: Bus,
        /// Its output, from 1.
        output: u16,
    },
    /// "Preset": shows the presets, or hides them.
    OpenPresetMenu,
    /// The preset for this many outputs, fitted to the device.
    Preset(u16),
    /// "Reset": the device's own preset.
    Reset,
}

/// What [`OutputPage::follow`] settles, as the Audio output screen shows
/// it: whether a buffer size or a routing still waits for the device, and
/// why the last ones were not kept. A turn of the screens' timer that
/// changes it draws them again ([`crate::tick::Drawn`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settling {
    buffer_waits: bool,
    routing_waits: bool,
    buffer_problem: Option<String>,
    routing_problem: Option<String>,
}

/// The device whose routing the screen edits.
struct Current {
    id: String,
    outputs: u16,
    routing: Routing,
}

/// The Audio output screen's own state: the devices listed, the menus,
/// the preset last applied and the last problem.
#[derive(Debug, Default)]
pub struct OutputPage {
    devices: Vec<DeviceInfo>,
    device_menu: bool,
    preset_menu: bool,
    /// Why the last buffer size was not kept.
    buffer_problem: Option<String>,
    /// Why the last routing was not kept.
    routing_problem: Option<String>,
    /// The buffer size waiting for the stream to open with it.
    pending_buffer: Option<PendingBuffer>,
    /// The routing waiting for the stream to open with its outputs.
    pending_routing: Option<PendingRouting>,
}

impl OutputPage {
    /// The screen with no device listed yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Settles the changes waiting for the stream to open again, from the
    /// session `show` as last polled and the screen's clock `now`: one the
    /// stream now plays with is kept (saved); one the device refused, was
    /// lost before, or plays otherwise (at once for a stream opened again
    /// without it, after [`APPLY_PATIENCE`] for the stream of before or a
    /// hold already there) is not, and says why. With no session they
    /// settle as [`Self::song_closed`] says. Called on every poll of the
    /// screen.
    pub fn follow(&mut self, show: Option<&mut Show>, settings: &mut AudioSettings, now: Instant) {
        let Some(show) = show else {
            self.song_closed();
            return;
        };
        let snapshot = show.snapshot();
        if let Some(pending) = &self.pending_buffer
            && let Some(verdict) = pending.verdict(&snapshot.device, now)
        {
            self.buffer_problem = match verdict {
                Verdict::Kept => {
                    settings.set_buffer_frames(pending.frames);
                    None
                }
                Verdict::NotKept(why) => Some(buffer_not_kept(why)),
            };
            self.pending_buffer = None;
        }
        let outputs = snapshot.mixer.routing.outputs;
        if let Some(pending) = &self.pending_routing
            && let Some(verdict) = pending.verdict(&snapshot.device, outputs, now)
        {
            self.routing_problem = match verdict {
                Verdict::Kept => save_routing(settings, &pending.device, &pending.routing),
                Verdict::NotKept(why) => Some(routing_not_kept(why)),
            };
            self.pending_routing = None;
        }
    }

    /// The song the changes waited on closed (or opened again, on another
    /// device or not) before the device opened with them: they are not
    /// kept, and say so. The screen calls it once that song has closed.
    pub fn song_closed(&mut self) {
        let closed = |device: &str| {
            format!("the song closed before audio device \"{device}\" opened with it")
        };
        if let Some(pending) = self.pending_buffer.take() {
            self.buffer_problem = Some(buffer_not_kept(closed(&pending.device)));
        }
        if let Some(pending) = self.pending_routing.take() {
            self.routing_problem = Some(routing_not_kept(closed(&pending.before.device)));
        }
    }

    /// What [`Self::follow`] has settled so far ([`Settling`]).
    pub fn settling(&self) -> Settling {
        Settling {
            buffer_waits: self.pending_buffer.is_some(),
            routing_waits: self.pending_routing.is_some(),
            buffer_problem: self.buffer_problem.clone(),
            routing_problem: self.routing_problem.clone(),
        }
    }

    /// The output devices the system lists now.
    pub fn set_devices(&mut self, devices: Vec<DeviceInfo>) {
        self.devices = devices;
    }

    /// The device in use: the one `show` plays on, else the one `settings`
    /// opens the next session on.
    fn current(&self, show: Option<&Show>, settings: &AudioSettings) -> Option<Current> {
        if let Some(show) = show {
            let snapshot = show.snapshot();
            let routing = snapshot.mixer.routing.clone();
            if let DeviceStatus::Connected(stream) = &snapshot.device {
                let outputs = self
                    .devices
                    .iter()
                    .find(|device| device.id == stream.device_id)
                    .map_or(routing.outputs, |device| device.output_channels);
                return Some(Current {
                    id: stream.device_id.clone(),
                    outputs,
                    routing,
                });
            }
        }
        settings.device(&self.devices).map(|device| Current {
            id: device.id.clone(),
            outputs: device.output_channels,
            routing: settings.routing_for(device),
        })
    }

    /// Does what `action` asks of the song `show` plays and the settings;
    /// returns whether the song must open again, on another device.
    pub fn act(
        &mut self,
        action: OutputAction,
        show: Option<&mut Show>,
        settings: &mut AudioSettings,
        now: Instant,
    ) -> bool {
        let current = self.current(show.as_deref(), settings);
        let routing = match action {
            OutputAction::OpenDeviceMenu => {
                self.device_menu = !self.device_menu;
                return false;
            }
            OutputAction::OpenPresetMenu => {
                self.preset_menu = !self.preset_menu;
                return false;
            }
            OutputAction::ChooseDevice(index) => {
                self.device_menu = false;
                let Some(device) = self.devices.get(index) else {
                    return false;
                };
                let changed = current.is_none_or(|current| current.id != device.id);
                settings.choose(device);
                return changed && show.is_some();
            }
            OutputAction::Buffer(frames) => {
                self.set_buffer(frames, show, settings, now);
                return false;
            }
            OutputAction::ToggleSplit => current.as_ref().map(|current| {
                let mut routing = current.routing.clone();
                routing.split_mode = !routing.split_mode;
                routing
            }),
            OutputAction::Cell { bus, output } => current
                .as_ref()
                .map(|current| click_cell(&current.routing, bus, output)),
            OutputAction::Preset(outputs) => {
                self.preset_menu = false;
                current
                    .as_ref()
                    .map(|current| Routing::preset(outputs).fit(current.outputs))
            }
            OutputAction::Reset => current
                .as_ref()
                .map(|current| Routing::preset(current.outputs)),
        };
        if let (Some(current), Some(routing)) = (current, routing) {
            self.apply(&current.id, routing, show, settings, now);
        }
        false
    }

    /// Plays `show` with buffers of `frames` frames, and keeps the size for
    /// the next sessions once the device plays with it. While a song plays
    /// the stream opens again only once it has ramped out, and a device
    /// held already is tried with the size later, so the verdict comes from
    /// later polls ([`Self::follow`]).
    fn set_buffer(
        &mut self,
        frames: u32,
        show: Option<&mut Show>,
        settings: &mut AudioSettings,
        now: Instant,
    ) {
        self.pending_buffer = None;
        let Some(show) = show else {
            self.buffer_problem = if (MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(&frames) {
                settings.set_buffer_frames(frames);
                None
            } else {
                Some(buffer_not_kept(RuntimeError::BufferSize { frames }))
            };
            return;
        };
        let before = stream(show).cloned();
        let device = device_name(&show.snapshot().device).to_owned();
        if let Some(error) = refusal(show.set_buffer_frames(frames)) {
            self.buffer_problem = Some(buffer_not_kept(error));
            return;
        }
        self.buffer_problem = None;
        if before
            .as_ref()
            .is_some_and(|stream| stream.buffer_frames == frames)
        {
            settings.set_buffer_frames(frames);
            return;
        }
        self.pending_buffer = Some(PendingBuffer {
            frames,
            since: now,
            device,
            before,
        });
        show.poll();
        self.follow(Some(show), settings, now);
    }

    /// Plays `show` with `routing` and saves it for the device `device`
    /// once the session took it ([`refusal`]: a full queue still counts):
    /// at once when the stream already has the outputs it needs, or when
    /// the device is held and opens with it once back ([`PendingRouting`]);
    /// else once the stream has opened again with them ([`Self::follow`]).
    fn apply(
        &mut self,
        device: &str,
        routing: Routing,
        show: Option<&mut Show>,
        settings: &mut AudioSettings,
        now: Instant,
    ) {
        self.pending_routing = None;
        let Some(show) = show else {
            self.routing_problem = save_routing(settings, device, &routing);
            return;
        };
        let channels = channels_for(&routing, show.snapshot().mixer.routing.outputs);
        let narrow = stream(show)
            .filter(|stream| stream.channels < channels)
            .cloned();
        if let Some(error) = refusal(show.set_routing(routing.clone())) {
            self.routing_problem = Some(routing_not_kept(error));
            return;
        }
        let Some(before) = narrow else {
            self.routing_problem = save_routing(settings, device, &routing);
            return;
        };
        self.routing_problem = None;
        self.pending_routing = Some(PendingRouting {
            device: device.to_owned(),
            routing,
            since: now,
            before,
        });
        show.poll();
        self.follow(Some(show), settings, now);
    }

    /// What the screen shows of the song `show` plays and the settings.
    pub fn view(&self, show: Option<&Show>, settings: &AudioSettings) -> OutputScreen {
        let current = self.current(show, settings);
        let stream = show.and_then(|show| match &show.snapshot().device {
            DeviceStatus::Connected(stream) => Some(stream.clone()),
            _ => None,
        });
        let label = |device: &DeviceInfo| {
            format!("{} · {}", device.name, outputs_text(device.output_channels))
        };
        let devices = self
            .devices
            .iter()
            .map(|device| DeviceRow {
                label: label(device),
                chosen: current
                    .as_ref()
                    .is_some_and(|current| current.id == device.id),
            })
            .collect();
        let device = current
            .as_ref()
            .and_then(|current| self.devices.iter().find(|device| device.id == current.id))
            .map_or_else(|| "No output device".to_owned(), label);
        let outputs = current.as_ref().map_or(2, |current| current.outputs);
        let routing = current.as_ref().map_or_else(
            || Routing::stereo(outputs),
            |current| current.routing.clone(),
        );
        let split = routing.split_mode && outputs >= 2;
        let rows = Bus::ALL
            .into_iter()
            .map(|bus| {
                let on = routing.destination(bus).outputs();
                RoutingRow {
                    bus,
                    name: bus.name(),
                    icon: bus_icon(bus),
                    cells: (1..=outputs).map(|output| on.contains(&output)).collect(),
                }
            })
            .collect();
        let presets = PRESETS
            .into_iter()
            .filter(|&preset| preset <= outputs)
            .map(|preset| (preset, outputs_text(preset)))
            .collect();
        let mut notices: Vec<String> = settings.notices().to_vec();
        if let Some(show) = show {
            let snapshot = show.snapshot();
            notices.extend(snapshot.last_error.as_ref().and_then(problem_notice));
            if let DeviceStatus::Disconnected {
                reopen_error: Some(error),
                ..
            } = &snapshot.device
            {
                notices.extend(output_notice(error));
            }
        }
        notices.extend(self.buffer_problem.clone());
        notices.extend(self.routing_problem.clone());
        OutputScreen {
            devices,
            device,
            device_state: show.map_or(DeviceState::Closed, |show| match show.snapshot().device {
                DeviceStatus::Connected(_) => DeviceState::Open,
                DeviceStatus::Disconnected { .. } | DeviceStatus::PlayerLost { .. } => {
                    DeviceState::Lost
                }
            }),
            device_menu: self.device_menu,
            sample_rate: stream.as_ref().map_or_else(
                || "Set by the song that plays".to_owned(),
                |stream| rate_text(stream.sample_rate),
            ),
            sample_rate_note: show
                .zip(stream.as_ref())
                .map(|(show, stream)| rate_note(stream.sample_rate, show.song().sample_rate)),
            buffer_sizes: BUFFER_SIZES,
            buffer: stream
                .as_ref()
                .map_or(settings.buffer_frames(), |stream| stream.buffer_frames),
            latency: stream.as_ref().map_or_else(String::new, |stream| {
                format!("{:.1} ms", stream.latency().as_secs_f64() * 1_000.0)
            }),
            split_mode: (outputs == 2).then_some(routing.split_mode),
            hint: if split {
                "Split mode is on: only outputs 1 and 2 are used"
            } else {
                "Tap a cell to send a bus to an output"
            },
            outputs,
            rows,
            preset: format!("Preset: {}", preset_name(&routing, outputs)),
            preset_menu: self.preset_menu,
            presets,
            applying: self.pending_buffer.is_some() || self.pending_routing.is_some(),
            notices,
        }
    }
}
