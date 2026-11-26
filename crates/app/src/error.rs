//! Why a session could not open, and what went wrong while it runs.

use std::fmt;
use std::time::Duration;

use engine::live::LiveError;
use library::LoadError;
use thiserror::Error;

use crate::output::{LossReason, MAX_BUFFER_FRAMES, MIN_BUFFER_FRAMES, OutputError};

/// Why [`crate::Session::open`] failed.
#[derive(Debug, Error)]
pub enum OpenError {
    /// The buffer size is outside what F8.5 allows.
    #[error(
        "the buffer size must be {MIN_BUFFER_FRAMES} to {MAX_BUFFER_FRAMES} frames, not {frames}"
    )]
    BufferSize {
        /// The requested frames.
        frames: u32,
    },
    /// The song folder could not be read or checked; [`LoadError::Invalid`]
    /// lists every problem.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// The window the disk keeps decoded ahead of the playhead would not
    /// cover the largest buffer: the song's rate is too low.
    #[error(
        "the song's sample rate ({sample_rate} Hz) is too low to stream: {ahead_frames} frames kept ahead of the playhead do not cover a buffer of {MAX_BUFFER_FRAMES} frames"
    )]
    RateTooLow {
        /// The song's rate.
        sample_rate: u32,
        /// Frames the stream keeps ahead of the playhead at that rate.
        ahead_frames: u64,
    },
    /// The player could not be built, such as for an unknown arrangement.
    #[error(transparent)]
    Live(#[from] LiveError),
    /// The audio device could not be opened.
    #[error(transparent)]
    Output(#[from] OutputError),
}

/// What went wrong while a session runs: returned by
/// [`crate::Session::send`] and kept as [`crate::Snapshot::last_error`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeError {
    /// Play waited for the disk to load the audio at the playhead, and it
    /// did not arrive in time (advisory A4).
    #[error(
        "the audio at the playhead did not load from disk within {:.1} s; playback did not start",
        .waited.as_secs_f64()
    )]
    StartTimeout {
        /// How long play waited.
        waited: Duration,
    },
    /// The stream on the device stopped for good (F8 acceptance); the
    /// transport is paused until the device opens again.
    #[error("{}", Lost { device, reason: *reason })]
    DeviceLost {
        /// Name of the device that played.
        device: String,
        /// Why the stream stopped.
        reason: LossReason,
    },
    /// The lost device could not be opened again for another reason than
    /// being absent, such as a device that came back at another sample
    /// rate; the session keeps trying and the transport stays paused.
    #[error("audio device \"{device}\" cannot be opened again: {error}; playback stays paused")]
    DeviceReopenFailed {
        /// Name of the device that played.
        device: String,
        /// Why the last try failed.
        error: OutputError,
    },
    /// The lost device came back with fewer outputs than the routing used,
    /// so the routing was fitted to them ([`domain::Routing::fit`]): the buses
    /// that no longer fit play on outputs 1 and 2 (output 1 on a one-output
    /// device).
    #[error(
        "audio device \"{device}\" came back with {outputs} output(s); the routing was fitted to them"
    )]
    RoutingRefitted {
        /// Name of the device.
        device: String,
        /// The outputs the routing plays on now.
        outputs: u16,
    },
    /// The disk thread could not decode some blocks (advisory A5).
    #[error("the disk could not decode {blocks} block(s) of audio; they play as silence")]
    DecodeFailed {
        /// Blocks that failed so far.
        blocks: u64,
    },
    /// A queued jump passed boundaries because its audio was not loaded
    /// (R1-001): the operator sees that it waits, and for how long.
    #[error(
        "the jump to {section} is waiting for its audio from disk; it has moved {deferrals} boundary(ies) on"
    )]
    JumpWaiting {
        /// Label of the target section.
        section: String,
        /// Boundaries it has passed.
        deferrals: u32,
    },
    /// The command queue to the audio thread is full.
    #[error("the audio thread has not taken the last commands yet; {command} was not sent")]
    QueueFull {
        /// The command that was not sent.
        command: String,
    },
    /// A buffer size outside what F8.5 allows.
    #[error(
        "the buffer size must be {MIN_BUFFER_FRAMES} to {MAX_BUFFER_FRAMES} frames, not {frames}"
    )]
    BufferSize {
        /// The requested frames.
        frames: u32,
    },
    /// A mixer command for a stem the song does not have.
    #[error("stem {stem} is not in the song, which has {stems} stem(s)")]
    UnknownStem {
        /// The requested stem.
        stem: usize,
        /// Stems in the song.
        stems: usize,
    },
    /// A mixer command with a volume that is neither silent nor a number of
    /// dB up to [`domain::MAX_VOLUME_DB`].
    #[error(
        "a volume is silent or at most {} dB, not {db} dB",
        domain::MAX_VOLUME_DB
    )]
    InvalidVolume {
        /// The requested volume in dB, as written.
        db: String,
    },
    /// A jump to a position outside the arrangement.
    #[error("position {position} is outside the arrangement, which has {positions} positions")]
    UnknownPosition {
        /// The requested position.
        position: usize,
        /// Positions in the arrangement.
        positions: usize,
    },
    /// The audio output never handed the player back after its stream
    /// closed, so this session cannot play any more.
    #[error("the audio output did not release the player; open the song again")]
    PlayerLost,
}

/// The message of [`RuntimeError::DeviceLost`]: what happened, then what
/// brings the device back.
struct Lost<'a> {
    device: &'a str,
    reason: LossReason,
}

impl fmt::Display for Lost<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device = self.device;
        match self.reason {
            LossReason::Unplugged => write!(
                f,
                "audio device \"{device}\" was unplugged; playback is paused until it is plugged back in"
            ),
            reason => write!(
                f,
                "audio device \"{device}\" stopped: {reason}; playback is paused until the device opens again"
            ),
        }
    }
}
