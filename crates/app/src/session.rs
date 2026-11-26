//! A session: one song, or the current item of a setlist, playing on one
//! audio device.

use std::mem;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use domain::{
    ArrangementMix, Bus, BusSettings, DEFAULT_ARRANGEMENT_ID, DEFAULT_SEAM_CROSSFADE, MixFile,
    MixValidationError, Routing, Volume, bus_for_stem,
};
use engine::live::{
    self, DECLICK_FRAMES, DEFAULT_FADE_SECONDS, Deck, DeckController, DeckStatus, FadeState, Gain,
    Handoff, JumpKind, JumpTiming, PANIC_MILLISECONDS, Player, SendError, Transport,
};
use library::{DiskMonitor, Gate};
use rtrb::Consumer;

use crate::error::{OpenError, RuntimeError};
use crate::item::Item;
use crate::mixer::{ItemMix, StemView};
use crate::output::{
    AudioOutput, DEFAULT_BUFFER_FRAMES, DeviceChoice, LossReason, MAX_BUFFER_FRAMES,
    MIN_BUFFER_FRAMES, OutputCheck, OutputError, OutputRequest, OutputStream, StreamHealth,
    StreamInfo,
};
use crate::renderer::{AudioRenderer, AudioState, Report};
use crate::routing::{Router, device_outputs};
use crate::song::SongInfo;

/// How long the session waits for a closed stream to hand the player back.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

/// How a session opens; [`Default`] gives the usual values.
#[derive(Clone)]
pub struct SessionOptions {
    /// Arrangement to play. Default: `default`.
    pub arrangement: String,
    /// Output device, by id or name as [`DeviceChoice::Named`] matches it;
    /// `None` for the system default output. Whichever device opens, the
    /// session comes back to that one device by its id when it is lost.
    /// Default: `None`.
    pub device: Option<String>,
    /// Frames per audio callback, [`MIN_BUFFER_FRAMES`] to
    /// [`MAX_BUFFER_FRAMES`] (F8.5). Default: [`DEFAULT_BUFFER_FRAMES`].
    pub buffer_frames: u32,
    /// Crossfade at seams where the source jumps, in frames. The player and
    /// the disk stream both take it from here, so the stream always keeps
    /// the audio a fading-out tail reads (advisory A3). Default:
    /// [`DEFAULT_SEAM_CROSSFADE`].
    pub seam_crossfade: u64,
    /// How long play waits for the disk to load the audio at the playhead
    /// before it gives up with [`RuntimeError::StartTimeout`]. Default: 5 s.
    pub start_timeout: Duration,
    /// How often a session whose device was lost tries to open it again.
    /// Default: 500 ms.
    pub reconnect_interval: Duration,
    /// Test and diagnostics hook passed to the disk thread; see
    /// [`library::Gate`]. Default: `None`.
    pub disk_gate: Option<Gate>,
    /// Where each bus plays (F8.2-F8.4): the device's saved routing
    /// (`library::ConfigStore::load_routing`) or [`Routing::preset`] for its
    /// outputs; `None` for plain stereo on outputs 1 and 2, exactly what
    /// sessions played before routing (mono on a one-output device).
    ///
    /// The session always fits it to the outputs the device has when it
    /// opens, and again when it comes back after a loss
    /// ([`Routing::fit`]), so the routing in use
    /// ([`MixerView::routing`]) never plays past them; the stream opens
    /// with the highest output that routing uses, two at least. The
    /// players render the routing's [`Routing::outputs`] (two for plain
    /// stereo) for the whole session, so a routing given later
    /// ([`Session::set_routing`]) plays on those outputs at most.
    /// Default: `None`.
    pub routing: Option<Routing>,
    /// The global bus and master settings (F7.5), as the shell keeps them
    /// (`library::ConfigStore::load_bus_settings`); they carry from song to
    /// song. Default: every bus and the master at unity
    /// ([`BusSettings::new`]).
    pub bus_settings: BusSettings,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            arrangement: DEFAULT_ARRANGEMENT_ID.to_owned(),
            device: None,
            buffer_frames: DEFAULT_BUFFER_FRAMES,
            seam_crossfade: DEFAULT_SEAM_CROSSFADE,
            start_timeout: Duration::from_secs(5),
            reconnect_interval: Duration::from_millis(500),
            disk_gate: None,
            routing: None,
            bus_settings: BusSettings::new(),
        }
    }
}

/// A command a shell sends with [`Session::send`] (S1, F6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    /// Starts or resumes playing. From stopped or paused, it first waits
    /// until the disk has the audio at the playhead (the snapshot says
    /// [`TransportState::Starting`]), at most
    /// [`SessionOptions::start_timeout`]; the playhead is where every
    /// command sent before it left it, such as the start after
    /// [`Command::BackToStart`].
    Play,
    /// Ramps out and holds the position.
    Pause,
    /// Ramps out and returns to the start.
    Stop,
    /// Moves to the start of the arrangement.
    BackToStart,
    /// Queues a jump to arrangement position `position` (F6.2).
    QueueJump {
        /// Index into [`SongInfo::positions`].
        position: usize,
    },
    /// Drops the queued jump or loop.
    CancelJump,
    /// Chooses when queued jumps execute: at the section end or the next bar.
    SetJumpTiming(JumpTiming),
    /// Repeats the current section once more (F6.3).
    LiveLoop,
    /// Repeats the current section until switched off (F6.3).
    SetInfinite(bool),
    /// Fades the track stems out, or back in (F6.4).
    ToggleFade,
    /// How long a full fade takes.
    SetFadeDuration(Duration),
    /// Fades everything out in 0.5 s and stops (F6.6).
    Panic,
    /// Sets the volume of stem `stem`, an index into [`MixerView::stems`]
    /// (F7.1). Like every mixer command, it ramps over 20 ms while the
    /// transport plays (F7.4) and changes the mix as it plays
    /// ([`MixerView::mix`]), not `mix.json`.
    SetStemVolume {
        /// Index into [`MixerView::stems`].
        stem: usize,
        /// Silent, or up to [`domain::MAX_VOLUME_DB`].
        volume: Volume,
    },
    /// Mutes or unmutes stem `stem` (F7.1).
    SetStemMute {
        /// Index into [`MixerView::stems`].
        stem: usize,
        /// Whether it is muted.
        mute: bool,
    },
    /// Solos stem `stem` or ends its solo (F7.1): while a stem is soloed,
    /// the stems that are not are silent, except click and guide (F7.3).
    SetStemSolo {
        /// Index into [`MixerView::stems`].
        stem: usize,
        /// Whether it is soloed.
        solo: bool,
    },
    /// Sends stem `stem` to `bus`, or back to the bus the rules table
    /// gives it with `None` (F7.2); it crossfades between the two buses'
    /// outputs.
    SetStemBus {
        /// Index into [`MixerView::stems`].
        stem: usize,
        /// The bus, or `None` for the rules table's.
        bus: Option<Bus>,
    },
    /// Sets the volume of `bus`, for every song (F7.1, F7.5).
    SetBusVolume {
        /// The bus.
        bus: Bus,
        /// Silent, or up to [`domain::MAX_VOLUME_DB`].
        volume: Volume,
    },
    /// Mutes or unmutes `bus`, for every song: every stem in it goes silent
    /// and comes back at its level (F7 acceptance).
    SetBusMute {
        /// The bus.
        bus: Bus,
        /// Whether it is muted.
        mute: bool,
    },
    /// Sets the master volume, for every song (F7.1).
    SetMasterVolume(Volume),
    /// "Reset to song mix": every stem of the song back to its defaults,
    /// at unity, unmuted, unsoloed and on its rules-table bus, ramped like
    /// any mixer change. The bus and master settings stay; the mix file is
    /// not written, so [`MixerView::mix_changed`] says whether this differs
    /// from what it holds.
    ResetMix,
}

/// What the transport is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// Silent at the start of the arrangement.
    Stopped,
    /// Play was pressed; waiting for the disk to load the audio at the
    /// playhead.
    Starting,
    /// Playing.
    Playing,
    /// Ramping out before holding the position.
    Pausing,
    /// Silent, holding the position.
    Paused,
    /// Ramping out before returning to the start.
    Stopping,
    /// Fading everything out before stopping.
    Panicking,
}

/// The jump or loop waiting for its boundary (F6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedView {
    /// Arrangement position it goes to.
    pub target: usize,
    /// Label of that position's section.
    pub target_label: String,
    /// A jump or a live loop.
    pub kind: JumpKind,
    /// Output frame where it executes.
    pub executes_at: u64,
    /// Output frames until it executes.
    pub frames_until: u64,
    /// Boundaries it has passed because its audio was not loaded.
    pub deferrals: u32,
}

/// The fade of the track stems.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FadeView {
    /// Direction and progress.
    pub state: FadeState,
    /// Gain on the track stems, 0.0 to 1.0.
    pub level: f32,
    /// How long a full fade takes.
    pub duration: Duration,
}

/// Where the audio goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceStatus {
    /// A device plays the session.
    Connected(StreamInfo),
    /// The stream on the device was lost; the transport is paused, and the
    /// session tries to open the device again every
    /// [`SessionOptions::reconnect_interval`].
    Disconnected {
        /// Name of the device that played.
        device: String,
        /// Why the stream was lost.
        reason: LossReason,
        /// Why the last try to open it again failed, when the device is
        /// there but refused, such as a device that came back at another
        /// sample rate; `None` while the device is absent.
        reopen_error: Option<OutputError>,
    },
    /// The output kept the player after its stream closed, so the session
    /// cannot play any more ([`RuntimeError::PlayerLost`]).
    PlayerLost {
        /// Name of the device that played.
        device: String,
    },
}

/// The mixer and the routing a session plays with, for a shell to draw and
/// save.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerView {
    /// The current item's stems, in package order: the order mixer
    /// commands number them in.
    pub stems: Vec<StemView>,
    /// The current arrangement's mix as it plays.
    pub mix: ArrangementMix,
    /// Whether [`Self::mix`] differs from the song's `mix.json` as it was
    /// read or last saved ([`Session::mix_saved`]).
    pub mix_changed: bool,
    /// Why the song's `mix.json` could not be used, so the default mix
    /// plays: unreadable, malformed, or a newer format. The file is left as
    /// it is.
    pub mix_error: Option<String>,
    /// The rules the song's `mix.json` breaks against the song; the mix is
    /// still used, and entries the song lacks are ignored.
    pub mix_problems: Vec<MixValidationError>,
    /// The global bus and master settings.
    pub buses: BusSettings,
    /// Whether [`Self::buses`] differ from [`SessionOptions::bus_settings`]
    /// or from what was last saved ([`Session::bus_settings_saved`]).
    pub buses_changed: bool,
    /// The routing in use: [`SessionOptions::routing`] (or
    /// [`Session::set_routing`]'s), or plain stereo, fitted to the device's
    /// outputs.
    pub routing: Routing,
}

impl Default for MixerView {
    /// No stems, every level at unity, plain stereo on outputs 1 and 2.
    fn default() -> Self {
        Self {
            stems: Vec::new(),
            mix: ArrangementMix::default(),
            mix_changed: false,
            mix_error: None,
            mix_problems: Vec::new(),
            buses: BusSettings::new(),
            buses_changed: false,
            routing: Routing::stereo(2),
        }
    }
}

/// The state of a session, for a shell to draw; [`Session::poll`] returns
/// one, and shells call it about 30 times a second.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Transport state.
    pub transport: TransportState,
    /// Output frames played since the last start.
    pub output_frame: u64,
    /// Next source frame to play: the playhead in samples.
    pub source_frame: u64,
    /// Bar of the playhead, from 1.
    pub bar: u64,
    /// Beat of the playhead within its bar, from 1.
    pub beat: u32,
    /// Current arrangement position.
    pub position: usize,
    /// Label of the current section.
    pub section: String,
    /// The queued jump or loop, if any.
    pub queued: Option<QueuedView>,
    /// When queued jumps execute.
    pub jump_timing: JumpTiming,
    /// Whether the current section repeats until switched off.
    pub infinite: bool,
    /// The fade of the track stems.
    pub fade: FadeView,
    /// Where the audio goes.
    pub device: DeviceStatus,
    /// The mixer and the routing.
    pub mixer: MixerView,
    /// Stem reads the disk had not served in time; each played silence.
    pub underruns: u64,
    /// Blocks the disk thread could not decode.
    pub decode_errors: u64,
    /// The most recent problem. A problem tied to a condition clears when
    /// the condition ends, unless a newer problem replaced it first; it then
    /// falls back to a decode failure seen earlier, if any (see below):
    ///
    /// - [`RuntimeError::JumpWaiting`]: when the jump executes, is cancelled
    ///   or is replaced by one that has not waited.
    /// - [`RuntimeError::StartTimeout`]: when play is sent again.
    /// - [`RuntimeError::DeviceLost`] and [`RuntimeError::DeviceReopenFailed`]:
    ///   when the device plays again; a reopen failure turns back into the
    ///   loss when the device is absent again.
    ///
    /// Events stay until a newer problem replaces them: a refused command
    /// ([`RuntimeError::QueueFull`], [`RuntimeError::UnknownPosition`]),
    /// a device back with fewer outputs ([`RuntimeError::RoutingRefitted`]),
    /// [`RuntimeError::PlayerLost`], which never ends, and
    /// [`RuntimeError::DecodeFailed`]. A decode failure never ends either:
    /// the disk does not decode a failed block again, so its audio plays as
    /// silence whenever the arrangement reaches it, and the disk may report
    /// it long before (prefetch heads are decoded early).
    /// [`Self::decode_errors`] keeps counting failed blocks.
    pub last_error: Option<RuntimeError>,
}

impl Snapshot {
    /// Whether a live loop is queued.
    pub fn live_loop(&self) -> bool {
        self.queued
            .as_ref()
            .is_some_and(|queued| queued.kind == JumpKind::Loop)
    }
}

/// Where the player is.
enum Link {
    /// A device calls the renderer that holds it.
    Streaming {
        stream: Box<dyn OutputStream>,
        home: Consumer<Box<AudioState>>,
    },
    /// The device was lost; the control thread holds it until the device is
    /// back.
    Held {
        state: Box<AudioState>,
        reason: LossReason,
        /// Why the last reopen failed, when the device was there.
        reopen_error: Option<OutputError>,
        retry_at: Instant,
    },
    /// The output kept the player; nothing plays any more.
    Lost,
}

/// A stream to open again once the transport has ramped out, so the
/// device never stops or starts mid-signal.
#[derive(Debug, Clone, Copy)]
struct Reopen {
    /// Play again once the stream is open.
    resume: bool,
    /// Messages sent when the ramp out was: a report that applied as many
    /// shows how it went.
    after: u64,
}

/// Where a play request stands.
#[derive(Debug, Clone, Copy)]
enum Start {
    /// Waiting for the sources to have the playhead ready.
    Waiting { deadline: Instant },
    /// Play is on its way to the audio thread.
    Sent,
}

/// One song package playing on one audio device: the use cases a shell
/// calls (S1). Shells send [`Command`]s and draw the [`Snapshot`]s
/// [`Self::poll`] returns; they never touch the audio thread.
pub struct Session {
    song: SongInfo,
    output: Box<dyn AudioOutput>,
    request: OutputRequest,
    /// Name of the device the session plays on.
    device: String,
    deck: DeckController,
    /// The current item, as the deck knows it.
    item: u64,
    /// The jump timing last sent, carried over to the next setlist item.
    jump_timing: JumpTiming,
    reports: triple_buffer::Output<Report>,
    monitor: DiskMonitor,
    link: Link,
    start: Option<Start>,
    start_timeout: Duration,
    reconnect_interval: Duration,
    fade_duration: Duration,
    last_error: Option<RuntimeError>,
    decode_errors_seen: u64,
    deferral_seen: Option<(usize, u32)>,
    /// Messages the deck controller accepted; a report that has applied as
    /// many describes the playhead they left.
    sent: u64,
    router: Router,
    /// The routing in use changed while the device was away: say so once
    /// it plays again.
    refit_pending: bool,
    /// The global bus and master settings, and as last saved.
    buses: BusSettings,
    buses_saved: BusSettings,
    /// The current item's mix.
    mix: ItemMix,
    /// The cued item's, and the one sent back last, by their deck numbers:
    /// a handoff makes one of them current.
    cued_mix: Option<(u64, ItemMix)>,
    withdrawn_mix: Option<(u64, ItemMix)>,
    /// The generated click's gain, as the players have it.
    click_gain: f32,
    /// The players do not have every gain the mix and the bus settings
    /// give yet (the queue to the audio thread was full): every poll sends
    /// what is missing until they do.
    gains_pending: bool,
    /// The stream opens again once this ramp out is over.
    reopen: Option<Reopen>,
    /// The operator's last transport command was a pause, stop or panic.
    halted: bool,
}

/// A song folder opened for streaming with its player built, ready to play
/// but on no device yet ([`Session::prepare`]). It holds the disk thread
/// and the player's buffers, nothing more: preparing the next song while
/// another one plays leaves that one's device alone, and
/// [`Self::start`] opens the device once it is free.
pub struct PreparedSong {
    info: SongInfo,
    deck: DeckController,
    reports: triple_buffer::Output<Report>,
    monitor: DiskMonitor,
    state: Box<AudioState>,
    request: OutputRequest,
    start_timeout: Duration,
    reconnect_interval: Duration,
    router: Router,
    mix: ItemMix,
    buses: BusSettings,
}

impl PreparedSong {
    /// `item` as the first item, `tag`, of a deck, on no device yet.
    pub(crate) fn new(item: Item, tag: u64, options: &SessionOptions) -> Self {
        let Item {
            info,
            mut player,
            controller: _,
            monitor,
            mut mix,
        } = item;
        let router = Router::new(options.routing.clone());
        player.set_routes(router.routes());
        mix.prime(&mut player, &options.bus_settings);
        let channels = player.channels();
        let (deck, controller) = Deck::new(player, tag, options.buffer_frames as usize);
        let first = Report {
            deck: deck.status(),
            ready: false,
            applied: 0,
        };
        let (input, reports) = triple_buffer::triple_buffer(&first);
        let mut state = Box::new(AudioState::new(deck, input, channels));
        state.publish();
        let request = OutputRequest {
            device: options.device.clone().map(DeviceChoice::Named),
            sample_rate: info.sample_rate,
            buffer_frames: options.buffer_frames,
            channels: router.needed(),
        };
        Self {
            info,
            deck: controller,
            reports,
            monitor,
            state,
            request,
            start_timeout: options.start_timeout,
            reconnect_interval: options.reconnect_interval,
            router,
            mix,
            buses: options.bus_settings.clone(),
        }
    }

    /// The song and the arrangement it will play.
    pub fn song(&self) -> &SongInfo {
        &self.info
    }

    /// Asks `output` whether it would take this song ([`AudioOutput::check`])
    /// before any stream opens: the device, the song's sample rate, the
    /// buffer size, and the channels of the routing fitted to the device.
    /// Nothing opens, so the device stays as it is.
    pub fn check(&self, output: &dyn AudioOutput) -> OutputCheck {
        let mut router = self.router.clone();
        if let Some(outputs) = device_outputs(output, self.request.device.as_ref()) {
            router.fit_to(outputs);
        }
        output.check(&OutputRequest {
            channels: router.needed(),
            ..self.request.clone()
        })
    }

    /// Opens the output device at the song's sample rate, with the routing
    /// fitted to its outputs, and starts the session, stopped at the start.
    /// When the device does not open, the prepared song is dropped and the
    /// error says why.
    pub fn start(self, output: Box<dyn AudioOutput>) -> Result<Session, OpenError> {
        let Self {
            info,
            deck,
            reports,
            monitor,
            state,
            request,
            start_timeout,
            reconnect_interval,
            router,
            mix,
            buses,
        } = self;
        let click_gain = buses.bus_gain(Bus::ClickGuide);
        let mut session = Session {
            song: info,
            output,
            request,
            device: String::new(),
            deck,
            item: 0,
            jump_timing: JumpTiming::default(),
            reports,
            monitor,
            link: Link::Lost,
            start: None,
            start_timeout,
            reconnect_interval,
            fade_duration: Duration::from_secs(u64::from(DEFAULT_FADE_SECONDS)),
            last_error: None,
            decode_errors_seen: 0,
            deferral_seen: None,
            sent: 0,
            router,
            refit_pending: false,
            buses_saved: buses.clone(),
            buses,
            mix,
            cued_mix: None,
            withdrawn_mix: None,
            click_gain,
            gains_pending: false,
            reopen: None,
            halted: false,
        };
        match session.open_held(state) {
            Ok((stream, home)) => {
                // Come back to this very device when it is lost, by its id:
                // not to whatever the default output is by then, nor to
                // another device with the same name.
                session.request.device = Some(DeviceChoice::Id(stream.info().device_id.clone()));
                session.device = stream.info().device.clone();
                session.link = Link::Streaming { stream, home };
                // Fitting to the device that opens first is the rule, not news.
                session.refit_pending = false;
                Ok(session)
            }
            Err((state, error)) => {
                drop(state);
                Err(error.into())
            }
        }
    }
}

impl Session {
    /// Opens the song folder `song_dir` for streaming, builds the player
    /// for `options.arrangement`, and opens the output device at the song's
    /// sample rate. The transport starts stopped. It is
    /// [`Self::prepare`] followed by [`PreparedSong::start`].
    pub fn open(
        song_dir: &Path,
        output: Box<dyn AudioOutput>,
        options: SessionOptions,
    ) -> Result<Self, OpenError> {
        Self::prepare(song_dir, options)?.start(output)
    }

    /// Opens the song folder `song_dir` for streaming and builds the player
    /// for `options.arrangement`, without touching any device: every check
    /// of the song, the buffer size and the arrangement happens here, so
    /// only the device itself can still refuse it in
    /// [`PreparedSong::start`].
    pub fn prepare(song_dir: &Path, options: SessionOptions) -> Result<PreparedSong, OpenError> {
        check_buffer(options.buffer_frames)?;
        let item = Item::open_song(song_dir, &options.arrangement, &options)?;
        Ok(PreparedSong::new(item, 0, &options))
    }

    /// The open song and the arrangement being played.
    pub fn song(&self) -> &SongInfo {
        &self.song
    }

    /// The output devices (F8.1).
    pub fn devices(&self) -> Result<Vec<crate::DeviceInfo>, crate::OutputError> {
        self.output.devices()
    }

    /// Watches the disk thread; for tests and diagnostics.
    pub fn disk_monitor(&self) -> DiskMonitor {
        self.monitor.clone()
    }

    /// Sends `command`. An error is also kept as
    /// [`Snapshot::last_error`].
    pub fn send(&mut self, command: Command) -> Result<(), RuntimeError> {
        let result = self.dispatch(command);
        if let Err(error) = &result {
            self.last_error = Some(error.clone());
        }
        result
    }

    /// Plays `routing` from now on (F8.2), fitted to the device's outputs
    /// and to the outputs the players render ([`SessionOptions::routing`]):
    /// while the transport plays, every bus moves over the 20 ms gain ramp.
    /// A routing that needs more outputs than the stream opened with
    /// opens the stream again with them: a playing transport pauses with
    /// its ramp first and plays on from where it held once the stream is
    /// open, so the device never stops or starts mid-signal; a device that
    /// refuses then is held as a lost one
    /// ([`RuntimeError::DeviceReopenFailed`]). Saving
    /// it for the device stays with the shell
    /// (`library::ConfigStore::save_routing`).
    pub fn set_routing(&mut self, routing: Routing) -> Result<(), RuntimeError> {
        let mut router = self.router.clone();
        if router.set(routing) {
            let result = self.forward(live::Command::SetRouting(router.routes()));
            if let Err(error) = &result {
                self.last_error = Some(error.clone());
            }
            result?;
        }
        self.router = router;
        if self.router.needed() > self.request.channels {
            self.reopen_stream();
        }
        Ok(())
    }

    /// Plays with a buffer of `frames` frames per callback from now on,
    /// [`MIN_BUFFER_FRAMES`] to [`MAX_BUFFER_FRAMES`] (F8.5); the latency it
    /// adds is [`StreamInfo::latency`]. The stream opens again with it: a
    /// playing transport pauses with its ramp first and plays on from where
    /// it held once the stream is open (a pause, stop or panic sent
    /// meanwhile wins), a silent one opens at once. The players stay as
    /// they are, since they render any buffer in pieces of the size they
    /// were built with. A device that
    /// refuses the size is held as a lost one, saying why
    /// ([`RuntimeError::DeviceReopenFailed`]), until a size it takes is set.
    pub fn set_buffer_frames(&mut self, frames: u32) -> Result<(), RuntimeError> {
        if !(MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(&frames) {
            let error = RuntimeError::BufferSize { frames };
            self.last_error = Some(error.clone());
            return Err(error);
        }
        if frames != self.request.buffer_frames {
            self.request.buffer_frames = frames;
            self.reopen_stream();
        }
        Ok(())
    }

    /// The song folder and the `mix.json` to write there to save the mix as
    /// it plays (`library::write_mix`): the file as it was read, with the
    /// current arrangement's mix replaced; `None` for a click-only tile.
    /// Call [`Self::mix_saved`] once it is written.
    pub fn mix_file(&self) -> Option<(std::path::PathBuf, MixFile)> {
        self.mix.to_save()
    }

    /// The mix as it plays was saved: [`MixerView::mix_changed`] compares
    /// with it from now on.
    pub fn mix_saved(&mut self) {
        self.mix.saved();
    }

    /// The global bus and master settings as they play, to save
    /// (`library::ConfigStore::save_bus_settings`); call
    /// [`Self::bus_settings_saved`] once they are.
    pub fn bus_settings(&self) -> &BusSettings {
        &self.buses
    }

    /// The bus settings as they play were saved: [`MixerView::buses_changed`]
    /// compares with them from now on.
    pub fn bus_settings_saved(&mut self) {
        self.buses_saved = self.buses.clone();
    }

    /// Moves the session on and returns its state: starts a play that waited
    /// for the disk, notices a lost device (pausing the transport) and opens
    /// it again when it is back, sends the players the gains a full queue
    /// kept from them, and records decode failures and waiting jumps as
    /// [`Snapshot::last_error`]. Shells call it about 30 times a
    /// second; nothing advances between calls.
    pub fn poll(&mut self) -> Snapshot {
        let (report, now) = self.begin_poll();
        self.end_poll(&report, now)
    }

    /// The first half of [`Self::poll`]: drops the players the deck handed
    /// back, watches the device, and reads the newest report.
    pub(crate) fn begin_poll(&mut self) -> (Report, Instant) {
        let now = Instant::now();
        while let Some(player) = self.deck.returned() {
            drop(player);
        }
        self.watch_device(now);
        if self.gains_pending {
            // The queue was full; it may have room now. Still full, the
            // rest waits for the next poll.
            let _ = self.send_gains();
        }
        (*self.reports.read(), now)
    }

    /// The second half of [`Self::poll`], on the report the first half read.
    pub(crate) fn end_poll(&mut self, report: &Report, now: Instant) -> Snapshot {
        self.advance_reopen(report);
        self.advance_start(report, now);
        self.watch_disk(report);
        self.snapshot(report)
    }

    fn dispatch(&mut self, command: Command) -> Result<(), RuntimeError> {
        match command {
            Command::Play => {
                self.halted = false;
                // Play waits for the stream that is about to open again.
                if let Some(reopen) = &mut self.reopen {
                    reopen.resume = true;
                    return Ok(());
                }
                return self.play();
            }
            Command::Pause | Command::Stop | Command::Panic => {
                self.halted = true;
                self.start = None;
                if let Some(reopen) = &mut self.reopen {
                    reopen.resume = false;
                }
            }
            _ => {}
        }
        let engine_command = match command {
            Command::Play => live::Command::Play,
            Command::Pause => live::Command::Pause,
            Command::Stop => live::Command::Stop,
            Command::BackToStart => live::Command::BackToStart,
            Command::QueueJump { position } => live::Command::QueueJump { position },
            Command::CancelJump => live::Command::CancelJump,
            Command::SetJumpTiming(timing) => live::Command::SetJumpTiming(timing),
            Command::LiveLoop => live::Command::LiveLoop,
            Command::SetInfinite(on) => live::Command::SetInfinite(on),
            Command::ToggleFade => live::Command::ToggleFade,
            Command::SetFadeDuration(duration) => live::Command::SetFadeDuration {
                frames: self.fade_frames(duration),
            },
            Command::Panic => live::Command::Panic,
            Command::SetStemVolume { .. }
            | Command::SetStemMute { .. }
            | Command::SetStemSolo { .. }
            | Command::SetStemBus { .. }
            | Command::SetBusVolume { .. }
            | Command::SetBusMute { .. }
            | Command::SetMasterVolume(_)
            | Command::ResetMix => return self.mixer(command),
        };
        self.forward(engine_command)?;
        if let Command::SetJumpTiming(timing) = command {
            self.jump_timing = timing;
        }
        if let Command::SetFadeDuration(duration) = command {
            let frames = self.fade_frames(duration).max(DECLICK_FRAMES);
            self.fade_duration =
                Duration::from_secs_f64(frames as f64 / f64::from(self.song.sample_rate));
        }
        Ok(())
    }

    /// Changes the mix or the bus settings as `command`, a mixer command,
    /// says, then sends the players what that changes. A command refused
    /// (an unknown stem, a volume out of range) changes nothing; one the
    /// full queue refuses returns [`RuntimeError::QueueFull`] but stands,
    /// and reaches the players from the next polls.
    fn mixer(&mut self, command: Command) -> Result<(), RuntimeError> {
        let valid = |volume: Volume| {
            if volume.is_valid() {
                Ok(())
            } else {
                Err(RuntimeError::InvalidVolume {
                    db: volume.db().to_string(),
                })
            }
        };
        let stem = |stem: usize| {
            let stems = self.mix.stems();
            stems.get(stem).cloned().ok_or(RuntimeError::UnknownStem {
                stem,
                stems: stems.len(),
            })
        };
        match command {
            Command::SetStemVolume {
                stem: index,
                volume,
            } => {
                valid(volume)?;
                let stem = stem(index)?;
                self.change_stem(&stem.id, |mix| mix.volume = volume);
            }
            Command::SetStemMute { stem: index, mute } => {
                let stem = stem(index)?;
                self.change_stem(&stem.id, |mix| mix.mute = mute);
            }
            Command::SetStemSolo { stem: index, solo } => {
                let stem = stem(index)?;
                self.change_stem(&stem.id, |mix| mix.solo = solo);
            }
            Command::SetStemBus { stem: index, bus } => {
                let stem = stem(index)?;
                // The rules table's bus is stored as no choice at all.
                let bus = bus.filter(|&bus| bus != bus_for_stem(&stem));
                self.change_stem(&stem.id, |mix| mix.set_bus(bus));
            }
            Command::SetBusVolume { bus, volume } => {
                valid(volume)?;
                let mut level = self.buses.level(bus);
                level.volume = volume;
                self.buses.set_level(bus, level);
            }
            Command::SetBusMute { bus, mute } => {
                let mut level = self.buses.level(bus);
                level.mute = mute;
                self.buses.set_level(bus, level);
            }
            Command::SetMasterVolume(volume) => {
                valid(volume)?;
                self.buses.master_volume = volume;
            }
            Command::ResetMix => self.mix.mix = domain::ArrangementMix::default(),
            _ => {}
        }
        self.send_gains()
    }

    fn change_stem(&mut self, id: &str, change: impl FnOnce(&mut domain::StemMix)) {
        let mut stem = self.mix.mix.stem(id);
        change(&mut stem);
        self.mix.mix.set_stem(id, stem);
    }

    /// Sends the current and the cued item the gains their mixes and the
    /// bus settings give, and every item the click's gain: only what
    /// changed, each item by its deck number, so a handoff on its way does
    /// not give one item's gains to another's stems. What the queue to the
    /// audio thread refuses stays to send: the mix and the bus settings
    /// keep what was asked, and every poll sends the rest until the players
    /// have it, so a burst of changes ends at the last one.
    fn send_gains(&mut self) -> Result<(), RuntimeError> {
        let mut result = Ok(());
        let current = self.item;
        let mut mix = mem::replace(&mut self.mix, ItemMix::click());
        result = result.and(self.send_mix(current, &mut mix));
        self.mix = mix;
        if let Some((tag, mut mix)) = self.cued_mix.take() {
            result = result.and(self.send_mix(tag, &mut mix));
            self.cued_mix = Some((tag, mix));
        }
        let click = self.buses.bus_gain(Bus::ClickGuide);
        if click != self.click_gain && result.is_ok() {
            result = self
                .push(live::Command::SetClickGain(Gain::new(click)))
                .map_err(queue_full);
            if result.is_ok() {
                self.click_gain = click;
            }
        }
        self.apply_if_held();
        self.gains_pending = result.is_err();
        result
    }

    /// Sends item `item` what takes it to `mix` with the bus settings.
    fn send_mix(&mut self, item: u64, mix: &mut ItemMix) -> Result<(), RuntimeError> {
        for (stem, target, command) in mix.changes(&self.buses) {
            self.deck.send_to(item, command).map_err(queue_full)?;
            self.sent += 1;
            mix.sent(stem, target, command);
        }
        Ok(())
    }

    fn fade_frames(&self, duration: Duration) -> u64 {
        (duration.as_secs_f64() * f64::from(self.song.sample_rate)).round() as u64
    }

    /// Play from stopped or paused waits until the sources have the playhead
    /// ready, so the first buffer is never a silent underrun (advisory A4).
    /// It decides on a report that describes the playhead every command
    /// sent so far left; right after back to start, the last report may
    /// still describe the old playhead, so play waits for the next one.
    fn play(&mut self) -> Result<(), RuntimeError> {
        if let Some(error) = self.device_error() {
            return Err(error);
        }
        if let Link::Lost = self.link {
            return Err(RuntimeError::PlayerLost);
        }
        self.clear_error(|error| matches!(error, RuntimeError::StartTimeout { .. }));
        let report = *self.reports.read();
        let silent = matches!(
            report.status().transport,
            Transport::Stopped | Transport::Paused
        );
        if silent && !self.can_start(&report) {
            if !matches!(self.start, Some(Start::Waiting { .. })) {
                self.start = Some(Start::Waiting {
                    deadline: Instant::now() + self.start_timeout,
                });
            }
            return Ok(());
        }
        self.forward(live::Command::Play)?;
        self.start = silent.then_some(Start::Sent);
        Ok(())
    }

    fn advance_start(&mut self, report: &Report, now: Instant) {
        let Some(start) = self.start else {
            return;
        };
        if report.status().transport.is_running() {
            self.start = None;
            return;
        }
        if let Start::Waiting { deadline } = start {
            if self.can_start(report) {
                self.start = Some(Start::Sent);
                if let Err(error) = self.forward(live::Command::Play) {
                    self.start = None;
                    self.last_error = Some(error);
                }
            } else if now >= deadline {
                self.start = None;
                self.last_error = Some(RuntimeError::StartTimeout {
                    waited: self.start_timeout,
                });
            }
        }
    }

    /// Whether `report` says the playhead is ready, and describes the
    /// playhead every command sent so far left.
    fn can_start(&self, report: &Report) -> bool {
        report.ready && report.applied >= self.sent
    }

    /// Queues `command` for the current item and counts it. A jump target
    /// is checked against the item's arrangement.
    fn push(&mut self, command: live::Command) -> Result<(), SendError> {
        let positions = self.song.positions.len();
        if let live::Command::QueueJump { position } = command
            && position >= positions
        {
            return Err(SendError::UnknownPosition {
                position,
                positions,
            });
        }
        self.deck.send(command)?;
        self.sent += 1;
        Ok(())
    }

    /// Clears the last error when `ended` says the condition behind it is
    /// over, falling back to a decode failure that still stands.
    fn clear_error(&mut self, ended: impl Fn(&RuntimeError) -> bool) {
        if self.last_error.as_ref().is_some_and(ended) {
            self.last_error = self.standing_error();
        }
    }

    /// A problem that outlives any newer one: failed blocks are never
    /// decoded again, so their audio stays silent for the session.
    fn standing_error(&self) -> Option<RuntimeError> {
        (self.decode_errors_seen > 0).then_some(RuntimeError::DecodeFailed {
            blocks: self.decode_errors_seen,
        })
    }

    /// Sends `command` to the player. While the device is lost the control
    /// thread holds the player, so it applies the command at once.
    fn forward(&mut self, command: live::Command) -> Result<(), RuntimeError> {
        self.push(command).map_err(queue_full)?;
        self.apply_if_held();
        Ok(())
    }

    /// While the device is lost the control thread holds the deck, so it
    /// applies what was sent at once.
    fn apply_if_held(&mut self) {
        if let Link::Held { state, .. } = &mut self.link {
            state.deck.process(&mut []);
            state.publish();
        }
    }

    fn watch_device(&mut self, now: Instant) {
        let (lost, retry) = match &self.link {
            Link::Streaming { stream, .. } => match stream.health() {
                StreamHealth::Lost { reason } => (Some(reason), false),
                StreamHealth::Running => (None, false),
            },
            Link::Held { retry_at, .. } => (None, now >= *retry_at),
            Link::Lost => (None, false),
        };
        if let Some(reason) = lost {
            self.device_lost(reason, now);
        } else if retry {
            self.reconnect(now);
        }
    }

    /// Takes the player back from the dead stream, pauses it at once, and
    /// holds it until the device is back (F8 acceptance).
    fn device_lost(&mut self, reason: LossReason, now: Instant) {
        let Link::Streaming { stream, home } = mem::replace(&mut self.link, Link::Lost) else {
            return;
        };
        drop(stream);
        self.start = None;
        // The device is gone: it opens with the session's request when it is
        // back, and the transport stays paused, as after any loss.
        self.reopen = None;
        let Some(mut state) = take_back(home) else {
            self.last_error = Some(RuntimeError::PlayerLost);
            return;
        };
        self.pause_now(&mut state);
        self.link = Link::Held {
            state,
            reason,
            reopen_error: None,
            retry_at: now + self.reconnect_interval,
        };
        self.last_error = self.device_error();
    }

    /// Pauses the held player on the control thread: no device is calling
    /// back, so the pause ramp renders into a buffer nobody hears. A stop or
    /// a panic under way finishes the same way.
    fn pause_now(&mut self, state: &mut AudioState) {
        state.deck.process(&mut []);
        let _ = self.push(live::Command::Pause);
        let max_frames = self.request.buffer_frames as usize;
        let mut scratch = vec![0.0f32; state.channels() * max_frames];
        let longest =
            u64::from(self.song.sample_rate) * PANIC_MILLISECONDS / 1_000 + DECLICK_FRAMES;
        for _ in 0..=longest.div_ceil(max_frames as u64) {
            state.deck.process(&mut scratch);
            if !state.deck.status().current.transport.is_running() {
                break;
            }
        }
        state.publish();
    }

    /// What keeps a held player from playing: the lost device, or why it
    /// did not open again.
    fn device_error(&self) -> Option<RuntimeError> {
        let Link::Held {
            reason,
            reopen_error,
            ..
        } = &self.link
        else {
            return None;
        };
        let device = self.device.clone();
        Some(match reopen_error {
            Some(error) => RuntimeError::DeviceReopenFailed {
                device,
                error: error.clone(),
            },
            None => RuntimeError::DeviceLost {
                device,
                reason: *reason,
            },
        })
    }

    /// Opens the lost device again, with the routing fitted to the outputs
    /// it has now; the transport stays paused. A failed try is reported only
    /// when its reason changes, so a newer problem is not overwritten at
    /// every retry.
    fn reconnect(&mut self, now: Instant) {
        let Link::Held {
            state,
            reason,
            reopen_error,
            ..
        } = mem::replace(&mut self.link, Link::Lost)
        else {
            return;
        };
        match self.open_held(state) {
            Ok((stream, home)) => self.connected(stream, home),
            Err((Some(state), error)) => {
                // An absent device is still lost for its first reason; one
                // that is there but refuses says why.
                let absent = is_absent(&error);
                let changed = reopen_error.as_ref() != (!absent).then_some(&error);
                self.link = Link::Held {
                    state,
                    reason,
                    reopen_error: (!absent).then_some(error),
                    retry_at: now + self.reconnect_interval,
                };
                if changed {
                    self.last_error = self.device_error();
                }
            }
            Err((None, _)) => self.last_error = Some(RuntimeError::PlayerLost),
        }
    }

    /// Opens the device with `state`, the deck held on this thread: fits
    /// the routing to the outputs the device has now (the players take it
    /// at once), then opens a stream with the channels that routing needs.
    /// On an error the state comes back, unless the output kept it.
    fn open_held(&mut self, mut state: Box<AudioState>) -> Opened {
        if let Some(outputs) = device_outputs(self.output.as_ref(), self.request.device.as_ref())
            && self.router.fit_to(outputs)
        {
            let _ = self.push(live::Command::SetRouting(self.router.routes()));
            state.deck.process(&mut []);
            state.publish();
            self.refit_pending = true;
        }
        self.request.channels = self.router.needed();
        state.set_out_channels(usize::from(self.request.channels));
        let (renderer, home) = AudioRenderer::new(state);
        match self.output.open(&self.request, renderer) {
            Ok(stream) => Ok((stream, home)),
            Err(error) => Err((take_back(home), error)),
        }
    }

    /// The device plays the session again: the loss is over, and a routing
    /// fitted to fewer outputs meanwhile is said.
    fn connected(&mut self, stream: Box<dyn OutputStream>, home: Consumer<Box<AudioState>>) {
        self.device = stream.info().device.clone();
        self.link = Link::Streaming { stream, home };
        self.clear_error(|error| {
            matches!(
                error,
                RuntimeError::DeviceLost { .. } | RuntimeError::DeviceReopenFailed { .. }
            )
        });
        if mem::take(&mut self.refit_pending) {
            self.last_error = Some(RuntimeError::RoutingRefitted {
                device: self.device.clone(),
                outputs: self.router.routing().outputs,
            });
        }
    }

    /// Opens the stream again with the session's request, without an edge:
    /// a transport that plays pauses first, with its ramp, holding the
    /// position, and plays on from there once the stream is open again
    /// ([`Self::advance_reopen`]); a silent one reopens at once. A held deck
    /// opens with the new request when the device is back.
    fn reopen_stream(&mut self) {
        if !matches!(self.link, Link::Streaming { .. }) {
            return;
        }
        if let Some(reopen) = &mut self.reopen {
            reopen.after = self.sent;
            return;
        }
        let transport = self.reports.read().status().transport;
        if !transport.is_running() {
            self.reopen_now();
            return;
        }
        // A pause, stop or panic on its way ends silent on its own.
        let resume = transport == Transport::Playing && !self.halted;
        if resume && let Err(error) = self.forward(live::Command::Pause) {
            // The queue is full: the stream reopens as it is.
            self.last_error = Some(error);
            self.reopen_now();
            return;
        }
        self.reopen = Some(Reopen {
            resume,
            after: self.sent,
        });
    }

    /// Opens the stream again once the report shows the ramp out over, and
    /// plays on when the transport played before it.
    fn advance_reopen(&mut self, report: &Report) {
        let Some(reopen) = self.reopen else {
            return;
        };
        if report.applied < reopen.after || report.status().transport.is_running() {
            return;
        }
        self.reopen = None;
        self.reopen_now();
        if reopen.resume
            && matches!(self.link, Link::Streaming { .. })
            && let Err(error) = self.play()
        {
            self.last_error = Some(error);
        }
    }

    /// Closes the stream and opens the device again with the session's
    /// request.
    fn reopen_now(&mut self) {
        let state = match mem::replace(&mut self.link, Link::Lost) {
            Link::Streaming { stream, home } => {
                drop(stream);
                take_back(home)
            }
            held => {
                // A held deck opens with the new request when the device is
                // back.
                self.link = held;
                return;
            }
        };
        match state {
            Some(state) => self.open_or_hold(state, LossReason::StreamInvalidated),
            None => self.last_error = Some(RuntimeError::PlayerLost),
        }
    }

    /// Opens the device with `state`; a device that refuses is held as a
    /// lost one, lost for `reason`, with the transport paused, and the
    /// session keeps trying.
    fn open_or_hold(&mut self, state: Box<AudioState>, reason: LossReason) {
        let now = Instant::now();
        match self.open_held(state) {
            Ok((stream, home)) => self.connected(stream, home),
            Err((Some(mut state), error)) => {
                if state.deck.status().current.transport.is_running() {
                    self.pause_now(&mut state);
                }
                self.link = Link::Held {
                    state,
                    reason,
                    reopen_error: (!is_absent(&error)).then_some(error),
                    retry_at: now + self.reconnect_interval,
                };
                self.last_error = self.device_error();
            }
            Err((None, _)) => self.last_error = Some(RuntimeError::PlayerLost),
        }
    }

    /// Records new decode failures, and new deferrals of the queued jump
    /// until the jump is over (see [`Snapshot::last_error`]).
    fn watch_disk(&mut self, report: &Report) {
        let status = report.status();
        let errors = self.monitor.decode_errors();
        if errors > self.decode_errors_seen {
            self.decode_errors_seen = errors;
            self.last_error = Some(RuntimeError::DecodeFailed { blocks: errors });
        }
        match status.queued {
            Some(queued) if queued.deferrals > 0 => {
                let seen = (queued.target, queued.deferrals);
                if self.deferral_seen != Some(seen) {
                    self.deferral_seen = Some(seen);
                    self.last_error = Some(RuntimeError::JumpWaiting {
                        section: self.song.label(queued.target).to_owned(),
                        deferrals: queued.deferrals,
                    });
                }
            }
            _ => {
                self.deferral_seen = None;
                self.clear_error(|error| matches!(error, RuntimeError::JumpWaiting { .. }));
            }
        }
    }

    fn snapshot(&self, report: &Report) -> Snapshot {
        let status = report.status();
        let transport = match status.transport {
            Transport::Stopped | Transport::Paused if self.start.is_some() => {
                TransportState::Starting
            }
            Transport::Stopped => TransportState::Stopped,
            Transport::Playing => TransportState::Playing,
            Transport::Pausing => TransportState::Pausing,
            Transport::Paused => TransportState::Paused,
            Transport::Stopping => TransportState::Stopping,
            Transport::Panicking => TransportState::Panicking,
        };
        let device = match &self.link {
            Link::Streaming { stream, .. } => DeviceStatus::Connected(stream.info().clone()),
            Link::Held {
                reason,
                reopen_error,
                ..
            } => DeviceStatus::Disconnected {
                device: self.device.clone(),
                reason: *reason,
                reopen_error: reopen_error.clone(),
            },
            Link::Lost => DeviceStatus::PlayerLost {
                device: self.device.clone(),
            },
        };
        Snapshot {
            transport,
            output_frame: status.output_frame,
            source_frame: status.source_frame,
            bar: status.bar_beat.bar,
            beat: status.bar_beat.beat,
            position: status.position,
            section: self.song.label(status.position).to_owned(),
            queued: status.queued.map(|queued| QueuedView {
                target: queued.target,
                target_label: self.song.label(queued.target).to_owned(),
                kind: queued.kind,
                executes_at: queued.executes_at,
                frames_until: queued.executes_at.saturating_sub(status.output_frame),
                deferrals: queued.deferrals,
            }),
            jump_timing: status.jump_timing,
            infinite: status.infinite,
            fade: FadeView {
                state: status.fade.state,
                level: status.fade.level,
                duration: self.fade_duration,
            },
            device,
            mixer: MixerView {
                stems: self.mix.views(),
                mix: self.mix.mix.clone(),
                mix_changed: self.mix.changed(),
                mix_error: self.mix.error().map(str::to_owned),
                mix_problems: self.mix.problems().to_vec(),
                buses: self.buses.clone(),
                buses_changed: self.buses != self.buses_saved,
                routing: self.router.routing().clone(),
            },
            underruns: status.underruns,
            decode_errors: self.monitor.decode_errors(),
            last_error: self.last_error.clone(),
        }
    }
}

/// What a setlist needs from the session it plays its items in.
impl Session {
    /// The current item, as the deck knows it.
    pub(crate) fn item(&self) -> u64 {
        self.item
    }

    /// The rate the output runs at.
    pub(crate) fn sample_rate(&self) -> u32 {
        self.request.sample_rate
    }

    /// The jump timing and fade duration the operator chose, for the next
    /// item to start with.
    pub(crate) fn carried(&self) -> (JumpTiming, Duration) {
        (self.jump_timing, self.fade_duration)
    }

    /// Messages the deck controller accepted so far: a report that has
    /// applied as many saw them all.
    pub(crate) fn sent(&self) -> u64 {
        self.sent
    }

    /// The deck as the newest report describes it.
    pub(crate) fn newest_deck(&mut self) -> DeckStatus {
        self.reports.read().deck
    }

    /// Whether the newest report has the current item's transport running.
    pub(crate) fn is_running(&mut self) -> bool {
        self.reports.read().status().transport.is_running()
    }

    /// The deck plays item `item` now: the snapshot describes it with
    /// `song`, and its disk is watched through `monitor`. A waiting play
    /// and the problems that belonged to the item before end with it.
    pub(crate) fn bind(&mut self, song: SongInfo, monitor: DiskMonitor, item: u64) {
        let handed_over = [&mut self.cued_mix, &mut self.withdrawn_mix]
            .into_iter()
            .find(|mix| mix.as_ref().is_some_and(|(tag, _)| *tag == item))
            .and_then(Option::take);
        self.song = song;
        self.monitor = monitor;
        self.item = item;
        self.start = None;
        self.decode_errors_seen = 0;
        self.deferral_seen = None;
        if matches!(
            self.last_error,
            Some(
                RuntimeError::DecodeFailed { .. }
                    | RuntimeError::JumpWaiting { .. }
                    | RuntimeError::StartTimeout { .. }
            )
        ) {
            self.last_error = None;
        }
        // An item the deck handed over to plays its own mix from now on.
        // One sent back first missed the bus changes since: they follow.
        if let Some((_, mix)) = handed_over {
            self.mix = mix;
            if let Err(error) = self.send_gains() {
                self.last_error = Some(error);
            }
        }
    }

    /// Cues `player` as item `item`, with its `mix`, to follow the current
    /// one with `handoff`. It takes the routing, the mix's gains and the bus
    /// settings first.
    pub(crate) fn cue(
        &mut self,
        mut player: Player,
        mut mix: ItemMix,
        item: u64,
        handoff: Handoff,
    ) -> Result<(), RuntimeError> {
        self.prime(&mut player, &mut mix);
        let sent = self.deck.cue(player, item, handoff);
        self.count(sent.is_ok(), "Cue")?;
        self.cued_mix = Some((item, mix));
        Ok(())
    }

    /// Sends the cued item back.
    pub(crate) fn uncue(&mut self) -> Result<(), RuntimeError> {
        let sent = self.deck.uncue();
        self.count(sent.is_ok(), "Uncue")?;
        if let Some(cued) = self.cued_mix.take() {
            self.withdrawn_mix = Some(cued);
        }
        Ok(())
    }

    /// Gives a player about to join the deck the routing in use, and its
    /// mix's gains with the bus settings.
    fn prime(&self, player: &mut Player, mix: &mut ItemMix) {
        player.set_routes(self.router.routes());
        mix.prime(player, &self.buses);
    }

    /// Makes `item` the current item as `tag`, stopped at its start; the
    /// item playing so far leaves with its stop ramp.
    pub(crate) fn select(&mut self, item: Item, tag: u64) -> Result<(), RuntimeError> {
        let Item {
            info,
            mut player,
            monitor,
            mut mix,
            ..
        } = item;
        self.prime(&mut player, &mut mix);
        let sent = self.deck.select(player, tag);
        self.count(sent.is_ok(), "Select")?;
        self.bind(info, monitor, tag);
        self.mix = mix;
        Ok(())
    }

    /// Makes the cued item, `tag`, current, stopped at its start.
    pub(crate) fn select_cued(
        &mut self,
        song: SongInfo,
        monitor: DiskMonitor,
        tag: u64,
    ) -> Result<(), RuntimeError> {
        let sent = self.deck.select_cued();
        self.count(sent.is_ok(), "SelectCued")?;
        self.bind(song, monitor, tag);
        Ok(())
    }

    /// Counts a message the deck accepted, or reports the full queue.
    fn count(&mut self, sent: bool, message: &str) -> Result<(), RuntimeError> {
        if !sent {
            let error = RuntimeError::QueueFull {
                command: message.to_owned(),
            };
            self.last_error = Some(error.clone());
            return Err(error);
        }
        self.sent += 1;
        self.apply_if_held();
        Ok(())
    }

    /// Plays `item` as `tag` at its own sample rate: makes `item` current,
    /// closes the stream (the current item has stopped), and opens the same
    /// device again at the new rate. A device that refuses is held as a lost
    /// one, so the session keeps trying and says why
    /// ([`RuntimeError::DeviceReopenFailed`]); choosing an item at a rate
    /// the device takes brings it back.
    ///
    /// The item goes to the deck before the stream closes: when the deck
    /// refuses it (its queue is full), nothing else changes, so the session
    /// keeps describing and playing the item it had at the rate it had, and
    /// the refusal is returned and kept as [`Snapshot::last_error`]. With no
    /// player left ([`RuntimeError::PlayerLost`]) nothing is sent at all.
    /// Should the output keep the player once the stream closes, the session
    /// is lost after all, still describing the item it had.
    pub(crate) fn reopen_with(&mut self, item: Item, tag: u64) -> Result<(), RuntimeError> {
        let Item {
            info,
            mut player,
            monitor,
            mut mix,
            ..
        } = item;
        self.prime(&mut player, &mut mix);
        let rate = info.sample_rate;
        // With no player left nothing is sent: the item is dropped here,
        // on the control thread, with its disk thread.
        if let Link::Lost = self.link {
            self.last_error = Some(RuntimeError::PlayerLost);
            return Err(RuntimeError::PlayerLost);
        }
        // Should the audio thread take it before the stream closes, the
        // item waits there stopped: silent, whatever the rate.
        let sent = self.deck.select(player, tag);
        self.count(sent.is_ok(), "Select")?;
        let state = match mem::replace(&mut self.link, Link::Lost) {
            Link::Streaming { stream, home } => {
                drop(stream);
                take_back(home)
            }
            Link::Held { state, .. } => Some(state),
            Link::Lost => None,
        };
        let Some(mut state) = state else {
            self.last_error = Some(RuntimeError::PlayerLost);
            return Err(RuntimeError::PlayerLost);
        };
        state.deck.process(&mut []);
        state.publish();
        self.bind(info, monitor, tag);
        self.mix = mix;
        self.request.sample_rate = rate;
        // This open takes any new request too.
        self.reopen = None;
        self.open_or_hold(state, LossReason::StreamInvalidated);
        match self.link {
            Link::Lost => Err(RuntimeError::PlayerLost),
            _ => Ok(()),
        }
    }
}

/// A stream opened with the session's deck, and the end its state comes
/// back through; or why it did not open, with the state back unless the
/// output kept it.
type Opened = Result<
    (Box<dyn OutputStream>, Consumer<Box<AudioState>>),
    (Option<Box<AudioState>>, OutputError),
>;

/// The error a full queue to the audio thread gives.
fn queue_full(error: SendError) -> RuntimeError {
    match error {
        SendError::Full(command) => RuntimeError::QueueFull {
            command: format!("{command:?}"),
        },
        SendError::UnknownPosition {
            position,
            positions,
        } => RuntimeError::UnknownPosition {
            position,
            positions,
        },
    }
}

/// Whether opening failed because the device is not there.
fn is_absent(error: &OutputError) -> bool {
    matches!(
        error,
        OutputError::NoDevice | OutputError::DeviceNotFound { .. }
    )
}

/// Refuses a buffer size outside what F8.5 allows.
pub(crate) fn check_buffer(frames: u32) -> Result<(), OpenError> {
    if (MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(&frames) {
        Ok(())
    } else {
        Err(OpenError::BufferSize { frames })
    }
}

impl Drop for Session {
    /// Stops the stream, takes the player back, and drops it here, on the
    /// control thread, which also waits for the disk thread to end.
    fn drop(&mut self) {
        match mem::replace(&mut self.link, Link::Lost) {
            Link::Streaming { stream, home } => {
                drop(stream);
                drop(take_back(home));
            }
            Link::Held { state, .. } => drop(state),
            Link::Lost => {}
        }
    }
}

/// Waits for a dropped renderer to hand its state back. When the output
/// keeps the renderer past [`RELEASE_TIMEOUT`], the queue is leaked so the
/// state can never be freed on the audio thread.
fn take_back(mut home: Consumer<Box<AudioState>>) -> Option<Box<AudioState>> {
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    loop {
        if let Ok(state) = home.pop() {
            return Some(state);
        }
        if Instant::now() >= deadline {
            mem::forget(home);
            return None;
        }
        thread::sleep(Duration::from_millis(1));
    }
}
