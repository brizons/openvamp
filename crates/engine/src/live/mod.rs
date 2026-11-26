//! Real-time live player (S2).
//!
//! A [`Player`] walks one arrangement of a [`domain::Song`] buffer by buffer
//! from [`Player::process`], which the audio callback calls. It keeps one
//! output frame counter, the current arrangement position, and the offset
//! into it. With no live command it plays exactly what [`crate::render_offline`]
//! renders for the compiled plan, seam crossfades and generated click
//! included, whatever the buffer size.
//!
//! A [`Controller`] sends [`Command`]s through a lock-free single-producer,
//! single-consumer queue; the player applies every pending command at the
//! start of the next [`Player::process`] call. A queued jump executes on the
//! exact boundary frame, splitting the buffer there when it falls inside one.
//! It executes only when every [`StemSource`] reports the target ready;
//! otherwise it stays queued and moves to the next boundary of the same kind,
//! so a jump never plays silence. A section end always executes: when the
//! arrangement enters the queued target there on its own, the queued jump is
//! dropped instead of playing the target twice. The player tells every source
//! where it is playing and where it may go next ([`StemSource::prefetch`]),
//! including both places a section end can go (its own start for a loop, and
//! the next position), so a streaming source can have the audio in memory in
//! time.
//!
//! A [`Deck`] plays a setlist on one audio thread: its current player and a
//! cued one, handed over sample-exactly at the current item's end (AutoLink)
//! or blended linearly over its last frames (Crossfade); see the [`Deck`]
//! docs. [`Player::set_end_fade`] fades an item's track stems out before its
//! end, and [`Player::set_infinite_at_start`] keeps a click-only tile's bar
//! repeating.
//!
//! # Mixer and router (F7, F8)
//!
//! Each stem has a live gain ([`Command::SetStemGain`], the effective gain
//! [`domain::effective_gains`] gives) and a bus ([`Command::SetStemBus`]);
//! the generated click plays on Click & Guide with its own gain
//! ([`Command::SetClickGain`]). Gains and routing changes ramp over
//! [`GAIN_RAMP_MILLISECONDS`]. The player renders
//! [`PlayerConfig::channels`] interleaved output channels, and [`Routes`] say
//! which outputs each bus plays on ([`Command::SetRouting`]).
//!
//! With every bus on outputs 1 and 2 and no master destination
//! ([`Routes::STEREO`], the default), the stems are summed in stem order
//! straight into outputs 1 and 2, as before routing existed: at unity gains
//! the samples are bit-identical to [`crate::render_offline`]'s, since every
//! gain the mixer adds multiplies by exactly 1.0. Any other routing sums each
//! stem into its bus (a stereo pair of buffers), then writes each bus to its
//! outputs.
//!
//! Real-time rules: `process` allocates nothing, takes no locks, and does no
//! I/O. Every buffer is sized at construction from the maximum buffer size
//! and the channel count, and every gain change (fade, click on/off, pause,
//! stop, panic, mixer gains, routing) ramps per frame.

mod deck;
mod meters;
mod player;
mod ramp;
mod source;
mod status;

use std::error::Error;
use std::fmt;

use domain::DEFAULT_SEAM_CROSSFADE;

pub use deck::{BlendStatus, Deck, DeckController, DeckError, DeckFull, DeckStatus, Handoff};
pub use meters::{MAX_METERED_STEMS, METER_WINDOW_MILLISECONDS, Meters};
pub use player::Player;
pub use source::{MemorySource, StemSource, memory_sources};
pub use status::{ClickSource, FadeState, FadeStatus, JumpKind, QueuedJump, Status, Transport};

/// Frames over which pause, stop, resume, and click on/off ramp, so the
/// transport never cuts a waveform mid-cycle. It is also the shortest fade.
pub const DECLICK_FRAMES: u64 = 256;

/// How long a panic takes to fade everything to silence (F6.6).
pub const PANIC_MILLISECONDS: u64 = 500;

/// Most prefetch targets the player passes to [`StemSource::prefetch`]: the
/// queued jump target, the start of the current position (where a loop
/// goes), the start of the next position, and the start of the arrangement.
pub const MAX_PREFETCH_TARGETS: usize = 4;

/// Default fade duration, in seconds of output ([`Command::SetFadeDuration`]
/// changes it).
pub const DEFAULT_FADE_SECONDS: u32 = 3;

/// Commands the queue holds before [`Controller::send`] reports it full.
pub const COMMAND_CAPACITY: usize = 256;

/// How long a gain or routing change takes to reach its target while the
/// transport runs (F7.4): every stem gain, the click gain and a routing
/// change ramp linearly over this time whatever the size of the change, so
/// a fader moved in steps never zips. While the transport is stopped or
/// paused a change applies at once, since nothing is heard.
pub const GAIN_RAMP_MILLISECONDS: u64 = 20;

/// Most output channels a player renders (F8.2).
pub const MAX_CHANNELS: u16 = domain::MAX_OUTPUTS;

/// A linear gain a [`Command`] carries, such as a stem's effective gain
/// ([`domain::effective_gains`]). Two gains are equal when their bits are,
/// so [`Command`] keeps `Eq`.
#[derive(Debug, Clone, Copy)]
pub struct Gain(f32);

impl Gain {
    /// `gain`, or 0.0 (silence) when it is negative or not a finite number.
    pub fn new(gain: f32) -> Self {
        Self(if gain.is_finite() && gain >= 0.0 {
            gain
        } else {
            0.0
        })
    }

    /// The linear factor.
    pub fn get(self) -> f32 {
        self.0
    }
}

impl PartialEq for Gain {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Gain {}

/// Where each bus plays, in a table of fixed size the audio thread takes
/// as it is ([`Command::SetRouting`]): one [`domain::Destination`] per bus
/// in [`domain::Bus::ALL`] order, and where the main mix also plays.
/// [`Routes::from_routing`] resolves a device's [`domain::Routing`], split
/// mode included.
///
/// A mono destination receives the bus's left and right folded as
/// `(left + right) / 2`, so a centred source keeps its level; buses sharing
/// an output are summed. The main mix is every bus but Click & Guide.
/// Outputs past the player's channel count play nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Routes {
    /// Destination of each bus, in [`domain::Bus::ALL`] order.
    pub buses: [domain::Destination; domain::Bus::ALL.len()],
    /// Where the main mix also plays, if anywhere.
    pub master: Option<domain::Destination>,
}

impl Routes {
    /// Every bus on outputs 1 and 2, no master: the routing that plays
    /// exactly what the players played before routing existed.
    pub const STEREO: Routes = Routes {
        buses: [domain::Destination::MAIN; domain::Bus::ALL.len()],
        master: None,
    };

    /// The table for `routing`: [`domain::Routing::destination`] of each
    /// bus (split mode resolved) and [`domain::Routing::master_destination`].
    /// Fit the routing to the device first ([`domain::Routing::fit`]).
    pub fn from_routing(routing: &domain::Routing) -> Routes {
        Routes {
            buses: domain::Bus::ALL.map(|bus| routing.destination(bus)),
            master: routing.master_destination(),
        }
    }
}

/// How a [`Player`] runs, besides the song, its mix, and its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerConfig {
    /// Output sample rate; it must be the song's rate.
    pub sample_rate: u32,
    /// Largest buffer [`Player::process`] is given; larger buffers are
    /// processed in pieces of this size.
    pub max_frames: usize,
    /// Crossfade at seams where the source jumps, as
    /// [`domain::CompileOptions::seam_crossfade`]; the same value gives the
    /// same samples as the compiled plan.
    pub seam_crossfade: u64,
    /// Output channels, 1 to [`MAX_CHANNELS`]: [`Player::process`] fills
    /// buffers of this many interleaved channels. Default 2.
    pub channels: u16,
}

impl PlayerConfig {
    /// `sample_rate` and `max_frames` with the default seam crossfade,
    /// [`DEFAULT_SEAM_CROSSFADE`], on 2 channels.
    pub fn new(sample_rate: u32, max_frames: usize) -> Self {
        Self {
            sample_rate,
            max_frames,
            seam_crossfade: DEFAULT_SEAM_CROSSFADE,
            channels: 2,
        }
    }
}

/// When a queued jump executes (F6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JumpTiming {
    /// At the end of the current arrangement position.
    #[default]
    SectionEnd,
    /// At the next bar line of the current section's source grid, or at its
    /// end when no bar line is left in it.
    NextBar,
}

/// A live command for the [`Player`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Starts playing. From [`Transport::Stopped`] the song starts at full
    /// level, like the offline render; from a pause it ramps back in over
    /// [`DECLICK_FRAMES`]. Ignored during a panic.
    Play,
    /// Ramps out over [`DECLICK_FRAMES`] and holds the position.
    Pause,
    /// Ramps out over [`DECLICK_FRAMES`], then returns to the start: position
    /// 0, output frame 0, no queued jump, infinite loop off, fade back in.
    Stop,
    /// Moves to the start of the arrangement. While audible it is a seam to
    /// position 0 with the equal-power crossfade; while paused it moves there
    /// silently.
    BackToStart,
    /// Queues a jump to arrangement position `position`, replacing any queued
    /// jump. It executes at the boundary [`JumpTiming`] selects, once every
    /// source has the target ready; until then it moves to the next boundary
    /// of the same kind ([`QueuedJump::deferrals`] counts the moves). When
    /// the arrangement enters `position` on its own at a section end (the
    /// next position, or the infinite loop), the jump is done and dropped.
    QueueJump {
        /// Index into the arrangement's section list.
        position: usize,
    },
    /// Drops the queued jump or loop.
    CancelJump,
    /// Chooses when queued jumps execute; a jump already queued follows the
    /// new timing.
    SetJumpTiming(JumpTiming),
    /// Repeats the current position once more: a queued jump to it at the
    /// end of the current position, whatever the [`JumpTiming`]. If a source
    /// does not have the position's start ready at that end, the arrangement
    /// plays on and the loop stays queued for the same position: it jumps
    /// back there at the end of the position then playing, or is dropped
    /// when the arrangement enters that position on its own (the infinite
    /// loop).
    LiveLoop,
    /// Repeats the current position at each of its ends until switched off.
    /// A queued jump takes priority; a jump to another position switches it
    /// off when it executes, a live loop or a jump to the current position
    /// does not.
    SetInfinite(bool),
    /// Fades the `track` stems out, or back in when they are faded or
    /// fading out. Click and guide stems and the generated click keep
    /// playing.
    ToggleFade,
    /// Sets how long a full fade takes, in frames; a fade from a partial
    /// level takes proportionally less. Durations shorter than
    /// [`DECLICK_FRAMES`] are raised to it, so a fade is never a step.
    SetFadeDuration {
        /// Frames for a full fade.
        frames: u64,
    },
    /// Fades everything, click included, to silence within
    /// [`PANIC_MILLISECONDS`], then stops like [`Command::Stop`].
    Panic,
    /// Turns the generated click on or off, ramping over [`DECLICK_FRAMES`].
    /// Like [`crate::ClickSettings::enabled`], it does not affect a `click`
    /// stem; [`Status::click_source`] says which click the song has.
    SetClick(bool),
    /// Sets the effective gain of stem `stem` (its index in the sources
    /// the player was built with), such as [`domain::StemGain::gain`]: the
    /// stem, bus and master volumes with mute and solo applied. It ramps over
    /// [`GAIN_RAMP_MILLISECONDS`] while the transport runs. The operator's
    /// fade and the end fade still apply on top, to `track` stems only. An
    /// unknown index is ignored.
    SetStemGain {
        /// Index of the stem.
        stem: usize,
        /// Its linear gain.
        gain: Gain,
    },
    /// Sends stem `stem` to `bus` ([`domain::StemGain::bus`]); it plays on
    /// that bus's outputs from the next frame. An unknown index is ignored.
    SetStemBus {
        /// Index of the stem.
        stem: usize,
        /// Its bus.
        bus: domain::Bus,
    },
    /// Sets the gain of the generated click, which plays on Click & Guide:
    /// [`domain::BusSettings::bus_gain`] of that bus, master included. It
    /// ramps like a stem gain and multiplies [`Command::SetClick`]'s on/off.
    SetClickGain(Gain),
    /// Routes the buses to the outputs. While the transport runs, the old
    /// and the new routing crossfade linearly over
    /// [`GAIN_RAMP_MILLISECONDS`], so no output jumps.
    SetRouting(Routes),
}

/// Sends [`Command`]s to its [`Player`] from the control thread.
#[derive(Debug)]
pub struct Controller {
    producer: rtrb::Producer<Command>,
    positions: usize,
}

impl Controller {
    /// Queues `command` for the player's next buffer.
    pub fn send(&mut self, command: Command) -> Result<(), SendError> {
        if let Command::QueueJump { position } = command
            && position >= self.positions
        {
            return Err(SendError::UnknownPosition {
                position,
                positions: self.positions,
            });
        }
        self.producer
            .push(command)
            .map_err(|rtrb::PushError::Full(command)| SendError::Full(command))
    }

    /// Number of positions in the player's arrangement.
    pub fn positions(&self) -> usize {
        self.positions
    }
}

/// Why a command was not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The queue holds [`COMMAND_CAPACITY`] commands the player has not
    /// applied yet.
    Full(Command),
    /// A jump target outside the arrangement.
    UnknownPosition {
        /// The requested position.
        position: usize,
        /// Number of positions in the arrangement.
        positions: usize,
    },
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(command) => write!(f, "the command queue is full; {command:?} was not sent"),
            Self::UnknownPosition {
                position,
                positions,
            } => write!(
                f,
                "position {position} is outside the arrangement, which has {positions} positions"
            ),
        }
    }
}

impl Error for SendError {}

/// Why a [`Player`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveError {
    /// The song has no arrangement with this id.
    UnknownArrangement {
        /// The requested id.
        id: String,
    },
    /// The output runs at another rate than the song; there is no resampling.
    SampleRateMismatch {
        /// The song's sample rate.
        song: u32,
        /// The requested output rate.
        output: u32,
    },
    /// The maximum buffer size is 0.
    ZeroBufferSize,
    /// The channel count is not 1 to [`MAX_CHANNELS`].
    ChannelsOutOfRange {
        /// The requested count.
        channels: u16,
    },
    /// [`crate::MixSettings::stem_gains`] names a stem with no source.
    UnknownStemGain {
        /// The id with a gain but no source.
        id: String,
    },
}

impl fmt::Display for LiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownArrangement { id } => write!(f, "unknown arrangement \"{id}\""),
            Self::SampleRateMismatch { song, output } => write!(
                f,
                "the song runs at {song} Hz but the output at {output} Hz; resampling is not supported"
            ),
            Self::ZeroBufferSize => write!(f, "the maximum buffer size must be at least one frame"),
            Self::ChannelsOutOfRange { channels } => write!(
                f,
                "a player renders 1 to {MAX_CHANNELS} output channels, not {channels}"
            ),
            Self::UnknownStemGain { id } => write!(
                f,
                "a gain is set for stem \"{id}\", but no stem source has that id"
            ),
        }
    }
}

impl Error for LiveError {}
