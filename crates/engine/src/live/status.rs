//! Plain-data snapshot of the live player.

use domain::BarBeat;

use super::JumpTiming;

/// What the transport is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Silent at the start of the arrangement.
    Stopped,
    /// Playing.
    Playing,
    /// Ramping out before holding the position.
    Pausing,
    /// Silent, holding the position.
    Paused,
    /// Ramping out before returning to the start.
    Stopping,
    /// Fading everything out before returning to the start.
    Panicking,
}

impl Transport {
    /// Whether the transport advances and produces audio.
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Playing | Self::Pausing | Self::Stopping | Self::Panicking
        )
    }
}

/// What a queued jump does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpKind {
    /// A jump to a chosen position, at the boundary the jump timing selects.
    Jump,
    /// A live loop: the current position again, at its end.
    Loop,
}

/// A jump waiting for its boundary (F6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedJump {
    /// Arrangement position it jumps to.
    pub target: usize,
    /// Jump or live loop.
    pub kind: JumpKind,
    /// Output frame where it executes, given the current position and jump
    /// timing.
    pub executes_at: u64,
    /// Boundaries it has already passed because a source did not have the
    /// target ready.
    pub deferrals: u32,
}

/// Where the click the musicians hear comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickSource {
    /// Synthesized from the tempo map; [`super::Command::SetClick`] turns it
    /// on and off.
    Generated,
    /// The song's `click` stem, which plays like any other stem;
    /// [`super::Command::SetClick`] does not affect it.
    Recorded,
}

/// Where the fade of the `track` stems stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeState {
    /// At full level.
    In,
    /// Ramping down.
    FadingOut,
    /// Silent.
    Out,
    /// Ramping up.
    FadingIn,
}

/// The fade of the `track` stems.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FadeStatus {
    /// Direction and progress.
    pub state: FadeState,
    /// Gain applied to the `track` stems, from 0.0 to 1.0.
    pub level: f32,
}

/// A snapshot of the live player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Status {
    /// Transport state.
    pub transport: Transport,
    /// Output frames played since the last start; it advances only while the
    /// transport runs and returns to 0 when the transport stops.
    pub output_frame: u64,
    /// Current arrangement position.
    pub position: usize,
    /// Next source frame to play.
    pub source_frame: u64,
    /// Bar and beat of [`Self::source_frame`] in the song's grid.
    pub bar_beat: BarBeat,
    /// The jump or loop waiting for its boundary.
    pub queued: Option<QueuedJump>,
    /// When queued jumps execute.
    pub jump_timing: JumpTiming,
    /// Whether the current position repeats until switched off.
    pub infinite: bool,
    /// The operator's fade of the `track` stems. An end fade set with
    /// `Player::set_end_fade` is a separate gain and does not show here.
    pub fade: FadeStatus,
    /// Whether the generated click is on.
    pub click: bool,
    /// Whether the song's click is generated or a recorded stem.
    pub click_source: ClickSource,
    /// Reads a stem source could not serve; each one played silence for
    /// that stem.
    pub underruns: u64,
    /// The peak levels of the stems, the buses and the master
    /// ([`super::Meters`]).
    pub meters: super::Meters,
}
