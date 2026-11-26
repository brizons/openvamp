//! What a session plays: one song, or one setlist item, with its player
//! built and its start buffering, ready to hand to the audio thread.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use domain::{ClickItem, DEFAULT_ARRANGEMENT_ID, SONG_FADE_OUT_SECONDS, Song};
use engine::MixSettings;
use engine::live::{self, Controller, JumpTiming, Player, PlayerConfig};
use library::{DiskMonitor, StreamConfig, StreamingSong, open_streaming};

use crate::error::OpenError;
use crate::mixer::ItemMix;
use crate::output::MAX_BUFFER_FRAMES;
use crate::session::SessionOptions;
use crate::song::SongInfo;

/// A player and what the control thread keeps about it.
pub(crate) struct Item {
    pub(crate) info: SongInfo,
    pub(crate) player: Player,
    /// The player's own queue, for settings applied before the player is
    /// handed over ([`Self::carry`]).
    pub(crate) controller: Controller,
    pub(crate) monitor: DiskMonitor,
    /// The arrangement's mix, from the song's `mix.json`; the session gives
    /// the player its gains before it plays.
    pub(crate) mix: ItemMix,
}

impl Item {
    /// Opens the song folder `dir` for streaming and builds the player for
    /// the whole `arrangement`: a single-song session's song. The player
    /// starts buffering at once.
    pub(crate) fn open_song(
        dir: &Path,
        arrangement: &str,
        options: &SessionOptions,
    ) -> Result<Self, OpenError> {
        let streaming = stream(dir, options)?;
        let mix = ItemMix::read(dir, &streaming.song, arrangement);
        Self::from_stream(streaming, arrangement, false, options, mix)
    }

    /// Opens a setlist song item: the song folder `dir`, its `arrangement`
    /// cut to positions `first..=last` when `positions` is given, and with
    /// `end_fade` the track stems fading out over the last
    /// [`SONG_FADE_OUT_SECONDS`] (F5.3). The setlist checked the positions
    /// against the song when it opened; when the song changed since and no
    /// longer has them, the item is refused ([`ItemError::Range`]) rather
    /// than played whole or cut somewhere else.
    pub(crate) fn open_item(
        dir: &Path,
        arrangement: &str,
        positions: Option<(usize, usize)>,
        end_fade: bool,
        options: &SessionOptions,
    ) -> Result<Self, ItemError> {
        let mut streaming = stream(dir, options)?;
        // Read against the whole song, which the file was written for.
        let mix = ItemMix::read(dir, &streaming.song, arrangement);
        if let Some(range) = positions {
            streaming.song = excerpt(streaming.song, arrangement, range)?;
        }
        Ok(Self::from_stream(
            streaming,
            arrangement,
            end_fade,
            options,
            mix,
        )?)
    }

    /// Builds the player for `arrangement` of the streamed song.
    fn from_stream(
        streaming: StreamingSong,
        arrangement: &str,
        end_fade: bool,
        options: &SessionOptions,
        mix: ItemMix,
    ) -> Result<Self, OpenError> {
        let StreamingSong {
            song,
            warnings,
            sources,
            monitor,
            ..
        } = streaming;
        let sample_rate = song.sample_rate();
        let (mut player, controller) = Player::new(
            &song,
            arrangement,
            &MixSettings::default(),
            &config(sample_rate, options),
            sources,
        )?;
        if end_fade {
            let frames = u64::from(SONG_FADE_OUT_SECONDS) * u64::from(sample_rate);
            player.set_end_fade(Some(frames));
        }
        let warnings = warnings.iter().map(ToString::to_string).collect();
        let info = SongInfo::new(&song, arrangement, warnings)
            .expect("`Player::new` accepted the arrangement");
        Ok(Self {
            info,
            player,
            controller,
            monitor,
            mix,
        })
    }

    /// The player of a click-only tile at `sample_rate` (F5.5): its bars,
    /// or one bar repeating until the operator moves on. `None` when the
    /// tile breaks a setlist rule.
    pub(crate) fn click(
        click: &ClickItem,
        sample_rate: u32,
        options: &SessionOptions,
    ) -> Option<Self> {
        let song = Song::click_only(sample_rate, click.bpm, click.time_signature, click.bars)?;
        let (mut player, controller) = Player::new(
            &song,
            DEFAULT_ARRANGEMENT_ID,
            &MixSettings::default(),
            &config(sample_rate, options),
            Vec::new(),
        )
        .ok()?;
        player.set_infinite_at_start(click.bars.is_none());
        let info = SongInfo::new(&song, DEFAULT_ARRANGEMENT_ID, Vec::new())?;
        Some(Self {
            info,
            player,
            controller,
            monitor: DiskMonitor::default(),
            mix: ItemMix::click(),
        })
    }

    /// Carries the operator's jump timing and fade duration over to this
    /// item, so a setting does not flip back when the next item starts. They
    /// apply here, on the control thread, before the player is handed over:
    /// a setting the operator sends later reaches the player afterwards and
    /// wins, instead of being undone by this one on the player's first
    /// buffer.
    pub(crate) fn carry(&mut self, timing: JumpTiming, fade: Duration) {
        let frames = (fade.as_secs_f64() * f64::from(self.info.sample_rate)).round() as u64;
        // The queue holds only what this method sends: there is room.
        let _ = self.controller.send(live::Command::SetJumpTiming(timing));
        let _ = self
            .controller
            .send(live::Command::SetFadeDuration { frames });
        // Takes both in without rendering anything.
        self.player.process(&mut []);
    }
}

fn config(sample_rate: u32, options: &SessionOptions) -> PlayerConfig {
    PlayerConfig {
        sample_rate,
        max_frames: options.buffer_frames as usize,
        seam_crossfade: options.seam_crossfade,
        channels: crate::routing::player_channels(options.routing.as_ref()),
    }
}

/// Why a setlist item could not be built.
#[derive(Debug)]
pub(crate) enum ItemError {
    /// The song folder, its stream, or its player.
    Open(OpenError),
    /// The song's arrangement no longer has the item's positions: the song
    /// changed after the setlist was checked.
    Range {
        arrangement: String,
        first: usize,
        last: usize,
        positions: usize,
    },
}

impl From<OpenError> for ItemError {
    fn from(error: OpenError) -> Self {
        Self::Open(error)
    }
}

impl fmt::Display for ItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => error.fmt(f),
            Self::Range {
                arrangement,
                first,
                last,
                positions,
            } => write!(
                f,
                "arrangement \"{arrangement}\" now has {positions} positions counted from 0, so positions {first} to {last} cannot play; the song changed after the setlist was checked"
            ),
        }
    }
}

/// Opens the song folder `dir` for streaming. One value feeds the player's
/// and the stream's seam crossfade, so the stream always keeps what the
/// player's seams read (advisory A3).
fn stream(dir: &Path, options: &SessionOptions) -> Result<StreamingSong, OpenError> {
    let stream_config = StreamConfig {
        seam_crossfade: options.seam_crossfade,
        gate: options.disk_gate.clone(),
        ..StreamConfig::default()
    };
    let streaming = open_streaming(dir, &stream_config)?;
    if streaming.capacity.ahead_frames < u64::from(MAX_BUFFER_FRAMES) {
        return Err(OpenError::RateTooLow {
            sample_rate: streaming.song.sample_rate(),
            ahead_frames: streaming.capacity.ahead_frames,
        });
    }
    Ok(streaming)
}

/// `song` cut to positions `first..=last` of `arrangement`. A song without
/// that arrangement is left whole, for [`Player::new`] to report; a range
/// the arrangement does not hold is refused.
fn excerpt(
    song: Song,
    arrangement: &str,
    (first, last): (usize, usize),
) -> Result<Song, ItemError> {
    let Some(positions) = song
        .arrangement(arrangement)
        .map(|arrangement| arrangement.sections.len())
    else {
        return Ok(song);
    };
    song.excerpt(arrangement, first, last)
        .ok_or_else(|| ItemError::Range {
            arrangement: arrangement.to_owned(),
            first,
            last,
            positions,
        })
}
