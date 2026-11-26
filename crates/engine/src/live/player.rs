//! The live player: walks an arrangement in real time.
//!
//! Every output frame is summed in the same order as [`crate::render_offline`]
//! sums it: for each stem in order, the fading-out tails of earlier seams
//! (oldest first) and then the current section; then the generated click
//! bursts in the order they started; then the master gain. The envelopes and
//! bursts are the offline renderer's own, and every live gain is exactly 1.0
//! at rest, so with no live command the samples are bit-identical. Seams use
//! [`domain::seam_fade`], the rule `compile` uses.

use std::mem;

use domain::{
    BeatGrid, Bus, DEFAULT_BUS, Destination, Song, StemGain, StemRole, bus_for_stem, seam_fade,
};
use rtrb::{Consumer, RingBuffer};

use super::meters::{MeterWindows, Meters, raise};
use super::ramp::Ramp;
use super::source::StemSource;
use super::status::{ClickSource, FadeState, FadeStatus, JumpKind, QueuedJump, Status, Transport};
use super::{
    COMMAND_CAPACITY, Command, Controller, DECLICK_FRAMES, DEFAULT_FADE_SECONDS,
    GAIN_RAMP_MILLISECONDS, Gain, JumpTiming, LiveError, MAX_CHANNELS, MAX_PREFETCH_TARGETS,
    PANIC_MILLISECONDS, PlayerConfig, Routes,
};
use crate::click::ClickBursts;
use crate::mix::Envelope;
use crate::{MixSettings, level_index};

/// Seams whose fade-out can overlap; a further seam drops the oldest tail.
const MAX_TAILS: usize = 8;

/// One arrangement position: a section's source range.
#[derive(Debug, Clone, Copy)]
struct Entry {
    src_start: u64,
    len: u64,
}

/// The section being played.
#[derive(Debug, Clone, Copy)]
struct Voice {
    position: usize,
    /// Frames played since entering this position.
    offset: u64,
    /// Length of the fade-in this voice plays (see `domain::Segment`).
    fade_in: u64,
    /// Fade-in frames already played when this position was entered: a
    /// contiguous seam carries the fade-in on instead of restarting it.
    env_start: u64,
}

impl Voice {
    const START: Self = Self {
        position: 0,
        offset: 0,
        fade_in: 0,
        env_start: 0,
    };

    /// Frame of the fade-in envelope the next frame plays.
    fn envelope_position(&self) -> u64 {
        self.env_start + self.offset
    }
}

/// The outgoing side of a seam: it keeps reading its source past the cut,
/// fading out under the incoming section.
#[derive(Debug, Clone, Copy)]
struct Tail {
    /// Next source frame.
    src_pos: u64,
    /// Fade-out length.
    len: u64,
    /// Frames of the fade-out played.
    played: u64,
    /// Fade-in of the cut voice and how far it had played, so a voice cut
    /// before its own fade-in finished fades out from where it was.
    fade_in: u64,
    cut_offset: u64,
}

impl Tail {
    fn remaining(&self) -> u64 {
        self.len - self.played
    }

    /// Envelope at frame `i` of the fade-out. A voice that finished its
    /// fade-in contributes exactly 1.0, as in a compiled plan.
    fn gain(&self, i: u64) -> f32 {
        Envelope::FadeIn(self.fade_in).at(self.cut_offset + i) * Envelope::FadeOut(self.len).at(i)
    }
}

struct Stem {
    source: Box<dyn StemSource>,
    gain: f32,
    /// Whether the fade applies: only `track` stems fade.
    fades: bool,
    /// The mixer's gain on the stem ([`Command::SetStemGain`]); 1.0 at rest.
    live: Ramp,
    /// The bus the stem sums into, or moves to ([`Command::SetStemBus`]).
    bus: Bus,
    /// How much of the stem each bus took when its last move began, and
    /// takes once it ends, in [`Bus::ALL`] order: a move crossfades between
    /// them as `bus_fade` goes from 0.0 to 1.0, so a move that starts
    /// during another one starts from the blend heard.
    bus_from: [f32; BUSES],
    bus_to: [f32; BUSES],
    /// 1.0 at rest.
    bus_fade: Ramp,
}

/// The whole stem on `bus`.
fn on_bus(bus: Bus) -> [f32; BUSES] {
    let mut weights = [0.0; BUSES];
    weights[bus.index()] = 1.0;
    weights
}

impl Stem {
    /// Sends the stem to `bus`: crossfading from where it is heard over
    /// `frames` frames while the transport is `audible`, at once otherwise.
    fn move_to(&mut self, bus: Bus, audible: bool, frames: u64) {
        if bus == self.bus {
            return;
        }
        let to = on_bus(bus);
        if audible {
            let reached = self.bus_fade.level();
            if self.bus_fade.is_done() {
                self.bus_from = self.bus_to;
            } else {
                for (from, to) in self.bus_from.iter_mut().zip(&self.bus_to) {
                    *from += (to - *from) * reached;
                }
            }
            self.bus_fade = Ramp::at(0.0);
            self.bus_fade.retarget_over(1.0, frames);
        } else {
            self.bus_from = to;
            self.bus_fade = Ramp::at(1.0);
        }
        self.bus_to = to;
        self.bus = bus;
    }

    /// Puts the stem on `bus` at once.
    fn place(&mut self, bus: Bus) {
        self.bus = bus;
        self.bus_from = on_bus(bus);
        self.bus_to = self.bus_from;
        self.bus_fade = Ramp::at(1.0);
    }

    /// The stem's gain at frame `i` of a block: its static gain, the
    /// mixer's (from `live`), and the fades for `track` stems. Each factor
    /// is exactly 1.0 at unity, so the product is the static gain.
    fn gain_at(&self, live: &[f32], track_gain: &[f32], i: usize) -> f32 {
        let gain = self.gain * live[i];
        if self.fades {
            gain * track_gain[i]
        } else {
            gain
        }
    }
}

/// Number of buses: [`Bus::ALL`].
const BUSES: usize = Bus::ALL.len();

/// What every stem of a block shares when it is mixed.
struct Block<'a> {
    n: usize,
    source: u64,
    tails: &'a [Tail],
    track_gain: &'a [f32],
    /// The master gain and transport ramp of each frame, for the meters.
    master: &'a [f32],
    /// The block's frames from `meter_from` to `meter_split` belong to the
    /// meters' window before the one under way once the block ends, those
    /// from `meter_split` on to that one, and earlier ones to neither.
    meter_from: usize,
    meter_split: usize,
    envelope: Envelope,
    envelope_start: u64,
}

/// Where a stem's meter readings go while it is mixed: its peaks in the
/// meters' two windows the block reaches (none past
/// [`super::MAX_METERED_STEMS`]), a bus meter buffer to add it to when its
/// bus is not a buffer of its own (the plain stereo path), and scratch for
/// the sum of its tails and its current section.
struct StemMeter<'a> {
    peaks: Option<(&'a mut [f32; 2], &'a mut [f32; 2])>,
    bus: Option<&'a mut [f32]>,
    heard: &'a mut [f32],
}

impl StemMeter<'_> {
    /// Raises the peak of frame `i` of the block to `left` and `right`.
    fn raise(&mut self, block: &Block<'_>, i: usize, left: f32, right: f32) {
        if let Some((before, after)) = &mut self.peaks
            && i >= block.meter_from
        {
            let peak = if i < block.meter_split {
                &mut **before
            } else {
                &mut **after
            };
            let master = block.master[i];
            raise(peak, left * master, right * master);
        }
    }
}

/// Adds `stem`'s fading-out tails, then its current section, `block.n`
/// frames, to the stereo buffer `out`, in that order: the order
/// [`crate::render_offline`] sums in. `live` and `read` are scratch. The
/// meter only reads what is added: `out` gets exactly the same samples.
fn add_stem(
    stem: &mut Stem,
    block: &Block<'_>,
    live: &mut [f32],
    read: &mut [f32],
    out: &mut [f32],
    underruns: &mut u64,
    mut meter: StemMeter<'_>,
) {
    let n = block.n;
    for value in &mut live[..n] {
        *value = stem.live.next();
    }
    // With tails, the stem's frames are the sum of two passes: the meter
    // keeps the first in `heard`.
    let tails = meter.peaks.is_some() && !block.tails.is_empty();
    let heard = mem::take(&mut meter.heard);
    let heard_frames = &mut heard[..2 * n];
    if tails {
        heard_frames.fill(0.0);
    }
    for tail in block.tails {
        let k = (n as u64).min(tail.remaining()) as usize;
        let read = &mut read[..2 * k];
        if !stem.source.read(tail.src_pos, read) {
            *underruns += 1;
            continue;
        }
        for i in 0..k {
            let gain = stem.gain_at(live, block.track_gain, i) * tail.gain(tail.played + i as u64);
            let (left, right) = (read[2 * i] * gain, read[2 * i + 1] * gain);
            out[2 * i] += left;
            out[2 * i + 1] += right;
            if let Some(bus) = meter.bus.as_deref_mut() {
                bus[2 * i] += left;
                bus[2 * i + 1] += right;
            }
            if tails {
                heard_frames[2 * i] += left;
                heard_frames[2 * i + 1] += right;
            }
        }
    }
    let read = &mut read[..2 * n];
    if !stem.source.read(block.source, read) {
        *underruns += 1;
        // Only the tails sounded.
        if tails {
            for (i, frame) in heard_frames.chunks_exact(2).enumerate() {
                meter.raise(block, i, frame[0], frame[1]);
            }
        }
        return;
    }
    for i in 0..n {
        let gain = stem.gain_at(live, block.track_gain, i)
            * block.envelope.at(block.envelope_start + i as u64);
        let (left, right) = (read[2 * i] * gain, read[2 * i + 1] * gain);
        out[2 * i] += left;
        out[2 * i + 1] += right;
        if let Some(bus) = meter.bus.as_deref_mut() {
            bus[2 * i] += left;
            bus[2 * i + 1] += right;
        }
        let (left, right) = if tails {
            (heard_frames[2 * i] + left, heard_frames[2 * i + 1] + right)
        } else {
            (left, right)
        };
        meter.raise(block, i, left, right);
    }
}

/// The channel index of output `output` (counted from 1), if the player
/// has it.
fn channel(output: u16, channels: usize) -> Option<usize> {
    let index = usize::from(output).checked_sub(1)?;
    (index < channels).then_some(index)
}

/// The router as gains: for each bus, side (left, right) and output
/// channel, at `(bus * 2 + side) * channels + channel`, how much of that
/// side plays there. A stereo pair takes each side whole, a mono output half
/// of each (the mean of left and right); the master destination adds every
/// bus but Click & Guide; outputs the player lacks get nothing.
fn route_matrix(routes: &Routes, channels: usize, matrix: &mut [f32]) {
    matrix.fill(0.0);
    let mut add = |bus: usize, destination: Destination| match destination {
        Destination::Stereo { left, right } => {
            for (side, output) in [(0, left), (1, right)] {
                if let Some(c) = channel(output, channels) {
                    matrix[(bus * 2 + side) * channels + c] += 1.0;
                }
            }
        }
        Destination::Mono(output) => {
            if let Some(c) = channel(output, channels) {
                matrix[(bus * 2) * channels + c] += 0.5;
                matrix[(bus * 2 + 1) * channels + c] += 0.5;
            }
        }
    };
    for (bus, &destination) in routes.buses.iter().enumerate() {
        add(bus, destination);
    }
    if let Some(master) = routes.master {
        let click = Bus::ClickGuide.index();
        for bus in (0..BUSES).filter(|&bus| bus != click) {
            add(bus, master);
        }
    }
}

/// A click burst still sounding.
#[derive(Debug, Clone, Copy)]
struct ActiveBurst {
    level: domain::ClickLevel,
    gain: f32,
    /// Next sample of the burst table.
    offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct Queued {
    target: usize,
    kind: JumpKind,
    /// First output frame the jump may execute on; after a deferral it is
    /// one past the boundary it missed.
    not_before: u64,
    deferrals: u32,
}

impl Queued {
    fn new(target: usize, kind: JumpKind) -> Self {
        Self {
            target,
            kind,
            not_before: 0,
            deferrals: 0,
        }
    }
}

/// The last prefetch hint sent to the sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hint {
    voice_epoch: u64,
    targets: [u64; MAX_PREFETCH_TARGETS],
    count: usize,
}

/// Plays one arrangement in real time; see the [module docs](super).
pub struct Player {
    entries: Vec<Entry>,
    /// `suffix[p]`: frames from the start of position `p` to the end of the
    /// arrangement; one more entry than `entries`, holding 0.
    suffix: Vec<u64>,
    song_length: u64,
    grid: BeatGrid,
    max_frames: usize,
    seam_crossfade: u64,
    panic_frames: u64,
    stems: Vec<Stem>,
    master_gain: f32,
    click_levels: [f32; 4],
    /// `None` when a `click` stem replaces the generated click.
    bursts: Option<ClickBursts>,
    /// Most click bursts that sound at once: at most one tick starts per
    /// output frame and a burst lasts its table length, plus one for the
    /// frame where the oldest ends.
    max_bursts: usize,
    commands: Consumer<Command>,

    // Scratch, sized at construction.
    read: Vec<f32>,
    track_gain: Vec<f32>,
    click_gain: Vec<f32>,
    master: Vec<f32>,
    active: Vec<ActiveBurst>,
    tails: Vec<Tail>,

    // Transport state.
    transport: Transport,
    transport_gain: Ramp,
    output_frame: u64,
    voice: Voice,
    queued: Option<Queued>,
    jump_timing: JumpTiming,
    infinite: bool,
    fade: Ramp,
    fade_out: bool,
    fade_frames: u64,
    click: Ramp,
    click_on: bool,
    underruns: u64,
    /// Counts every re-entry of a position, so hints follow playhead jumps.
    voice_epoch: u64,
    last_hint: Option<Hint>,
    /// Commands taken from the queue so far.
    applied: u64,
    /// Times the arrangement played to its end (not a stop or a panic).
    ends: u64,
    /// Frames before the end over which the track stems fade out.
    end_fade: Option<u64>,
    /// The end fade's own gain on the track stems, on top of the operator's
    /// fade; 1.0 at rest.
    end_gain: Ramp,
    /// Whether the end fade is running: the item can end within its window.
    end_fading: bool,
    /// Whether the infinite loop is on after every return to the start.
    infinite_at_start: bool,

    // Mixer and router.
    channels: usize,
    /// [`GAIN_RAMP_MILLISECONDS`] in frames.
    gain_ramp: u64,
    /// The generated click's gain ([`Command::SetClickGain`]).
    click_level: Ramp,
    routes: Routes,
    /// The router's gains ([`route_matrix`]) when the last routing change
    /// began, and once it ends: a change crossfades between them as
    /// `route_fade` goes from 0.0 to 1.0, so a change that starts during
    /// another one starts from the blend heard.
    route_from: Vec<f32>,
    route_to: Vec<f32>,
    /// 1.0 at rest.
    route_fade: Ramp,
    /// Per-frame gain of the stem being mixed.
    live: Vec<f32>,
    /// Per-frame level of the routing crossfade.
    route_gain: Vec<f32>,
    /// The stems in stereo on more than two channels with the stereo
    /// routing.
    mix: Vec<f32>,
    /// One stem's mix while it moves between buses.
    stem_mix: Vec<f32>,
    /// One stereo buffer per bus, in [`Bus::ALL`] order.
    buses: Vec<f32>,
    /// The meters' windows, and the peaks of the block being rendered in
    /// the two windows it reaches ([`MeterWindows::segments`]).
    meters: MeterWindows,
    block_meters: [Meters; 2],
    meter_segments: (usize, usize),
    /// The buses as the meters read them on the plain stereo path, where
    /// the stems sum straight into outputs 1 and 2; laid out as `buses`.
    meter_buses: Vec<f32>,
    /// One stem's tails, for its meter.
    meter_heard: Vec<f32>,
    /// Frames rendered in the current [`Player::process`] call: the others
    /// were silent.
    rendered: u64,
}

impl Player {
    /// Builds a stopped player for the arrangement `arrangement_id` of
    /// `song`, and the [`Controller`] that drives it.
    ///
    /// Stems are mixed in `sources` order with `settings`, as
    /// [`crate::render_offline`] mixes them. Everything the audio thread
    /// needs is allocated here, and every source gets its first
    /// [`StemSource::prefetch`] hint: the start of the arrangement.
    pub fn new(
        song: &Song,
        arrangement_id: &str,
        settings: &MixSettings,
        config: &PlayerConfig,
        sources: Vec<Box<dyn StemSource>>,
    ) -> Result<(Self, Controller), LiveError> {
        let PlayerConfig {
            sample_rate,
            max_frames,
            seam_crossfade,
            channels,
        } = *config;
        let arrangement =
            song.arrangement(arrangement_id)
                .ok_or_else(|| LiveError::UnknownArrangement {
                    id: arrangement_id.to_owned(),
                })?;
        if sample_rate != song.sample_rate() {
            return Err(LiveError::SampleRateMismatch {
                song: song.sample_rate(),
                output: sample_rate,
            });
        }
        if max_frames == 0 {
            return Err(LiveError::ZeroBufferSize);
        }
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(LiveError::ChannelsOutOfRange { channels });
        }
        if let Some(id) = settings
            .stem_gains
            .keys()
            .find(|id| !sources.iter().any(|source| source.id() == id.as_str()))
        {
            return Err(LiveError::UnknownStemGain { id: id.clone() });
        }

        let entries: Vec<Entry> = arrangement
            .sections
            .iter()
            .map(|id| {
                let section = song.section(id).expect(
                    "`validate` guarantees every arrangement section id names a section of the song",
                );
                Entry {
                    src_start: section.start,
                    len: section.length(),
                }
            })
            .collect();
        let has_click_stem = sources
            .iter()
            .any(|source| source.role() == StemRole::Click);
        let bursts = (!has_click_stem).then(|| ClickBursts::new(sample_rate));
        let max_bursts = bursts.as_ref().map_or(0, |bursts| bursts.len() + 1);
        let bus_of = |source: &dyn StemSource| match song
            .stems()
            .iter()
            .find(|stem| stem.id == source.id())
        {
            Some(stem) => bus_for_stem(stem),
            None if matches!(source.role(), StemRole::Click | StemRole::Guide) => Bus::ClickGuide,
            None => DEFAULT_BUS,
        };
        let stems: Vec<Stem> = sources
            .into_iter()
            .map(|source| {
                let bus = bus_of(source.as_ref());
                Stem {
                    gain: settings.stem_gain(source.id()),
                    fades: source.role() == StemRole::Track,
                    live: Ramp::at(1.0),
                    bus,
                    bus_from: on_bus(bus),
                    bus_to: on_bus(bus),
                    bus_fade: Ramp::at(1.0),
                    source,
                }
            })
            .collect();
        let stem_count = stems.len();
        let (producer, commands) = RingBuffer::new(COMMAND_CAPACITY);
        let click_level = if settings.click.enabled { 1.0 } else { 0.0 };
        let positions = entries.len();
        let mut suffix = vec![0; positions + 1];
        for (index, entry) in entries.iter().enumerate().rev() {
            suffix[index] = suffix[index + 1] + entry.len;
        }

        let mut player = Self {
            entries,
            suffix,
            song_length: song.length(),
            grid: BeatGrid::new(song),
            max_frames,
            seam_crossfade,
            panic_frames: u64::from(sample_rate) * PANIC_MILLISECONDS / 1_000,
            stems,
            master_gain: settings.master_gain,
            click_levels: settings.click.levels,
            bursts,
            max_bursts,
            commands,
            read: vec![0.0; 2 * max_frames],
            track_gain: vec![0.0; max_frames],
            click_gain: vec![0.0; max_frames],
            master: vec![0.0; max_frames],
            active: Vec::with_capacity(max_bursts),
            tails: Vec::with_capacity(MAX_TAILS),
            transport: Transport::Stopped,
            transport_gain: Ramp::at(1.0),
            output_frame: 0,
            voice: Voice::START,
            queued: None,
            jump_timing: JumpTiming::default(),
            infinite: false,
            fade: Ramp::at(1.0),
            fade_out: false,
            fade_frames: u64::from(DEFAULT_FADE_SECONDS) * u64::from(sample_rate),
            click: Ramp::at(click_level),
            click_on: settings.click.enabled,
            underruns: 0,
            voice_epoch: 0,
            last_hint: None,
            applied: 0,
            ends: 0,
            end_fade: None,
            end_gain: Ramp::at(1.0),
            end_fading: false,
            infinite_at_start: false,
            channels: usize::from(channels),
            gain_ramp: (u64::from(sample_rate) * GAIN_RAMP_MILLISECONDS / 1_000).max(1),
            click_level: Ramp::at(1.0),
            routes: Routes::STEREO,
            route_from: vec![0.0; BUSES * 2 * usize::from(channels)],
            route_to: vec![0.0; BUSES * 2 * usize::from(channels)],
            route_fade: Ramp::at(1.0),
            live: vec![0.0; max_frames],
            route_gain: vec![0.0; max_frames],
            mix: vec![0.0; 2 * max_frames],
            stem_mix: vec![0.0; 2 * max_frames],
            buses: vec![0.0; BUSES * 2 * max_frames],
            meters: MeterWindows::new(sample_rate, stem_count),
            block_meters: [Meters::SILENT; 2],
            meter_segments: (0, 0),
            meter_buses: vec![0.0; BUSES * 2 * max_frames],
            meter_heard: vec![0.0; 2 * max_frames],
            rendered: 0,
        };
        player.set_routes(Routes::STEREO);
        player.update_hints();
        Ok((
            player,
            Controller {
                producer,
                positions,
            },
        ))
    }

    /// Applies every pending command, then fills `out` with frames of
    /// [`PlayerConfig::channels`] interleaved channels (a trailing partial
    /// frame is left at 0.0). Real-time safe: no allocation, no lock, no I/O.
    pub fn process(&mut self, out: &mut [f32]) {
        while let Ok(command) = self.commands.pop() {
            self.apply(command);
            self.applied += 1;
        }
        self.update_hints();
        out.fill(0.0);
        let usable = out.len() - out.len() % self.channels;
        let piece = self.channels * self.max_frames;
        self.rendered = 0;
        for chunk in out[..usable].chunks_mut(piece) {
            self.render(chunk);
        }
        let frames = (usable / self.channels) as u64;
        self.meters.silence(frames - self.rendered);
    }

    /// A snapshot of the transport.
    pub fn status(&self) -> Status {
        let source_frame = self.source_frame();
        Status {
            transport: self.transport,
            output_frame: self.output_frame,
            position: self.voice.position,
            source_frame,
            bar_beat: self.grid.bar_and_beat(source_frame),
            queued: self.queued.map(|queued| QueuedJump {
                target: queued.target,
                kind: queued.kind,
                executes_at: self.jump_frame(queued),
                deferrals: queued.deferrals,
            }),
            jump_timing: self.jump_timing,
            infinite: self.infinite,
            fade: FadeStatus {
                state: match (self.fade_out, self.fade.is_done()) {
                    (false, true) => FadeState::In,
                    (true, false) => FadeState::FadingOut,
                    (true, true) => FadeState::Out,
                    (false, false) => FadeState::FadingIn,
                },
                level: self.fade.level(),
            },
            click: self.click_on,
            click_source: if self.bursts.is_some() {
                ClickSource::Generated
            } else {
                ClickSource::Recorded
            },
            meters: self.meters.published(),
            underruns: self.underruns,
        }
    }

    /// Fades the `track` stems out over the last `frames` frames before the
    /// arrangement ends, so they are silent on its last frame (F5.3). Click
    /// and guide stems and the generated click keep playing to the end.
    ///
    /// The window is the last `frames` frames as the player predicts them,
    /// the earliest the arrangement can end: honouring a queued jump or
    /// loop, but also where it ends should that jump wait for its audio.
    /// The fade starts while playing, exactly `frames` before that end. When
    /// the end moves out of the window (a jump back or a loop executes, or
    /// the infinite loop comes on), the fade is cancelled: the stems return
    /// over [`super::DECLICK_FRAMES`] (for a full swing) to the level of the
    /// operator's fade, and the fade starts again before the new end. When
    /// the end moves closer, the fade speeds up to stay silent on the last
    /// frame.
    ///
    /// It is a gain of its own on the stems, on top of the operator's
    /// [`Command::ToggleFade`]: the stems play at the operator's level times
    /// the end fade, so a fade the operator engaged stays engaged whatever
    /// the end fade does, and [`Status::fade`] reports the operator's fade
    /// only. `None`, the default, plays the end as it is. Call it before the
    /// player plays.
    pub fn set_end_fade(&mut self, frames: Option<u64>) {
        self.end_fade = frames;
    }

    /// Switches the infinite loop on or off now and at every return to the
    /// start (a stop, a panic), as [`Command::SetInfinite`] would right after
    /// it. A click-only tile without a bar count uses it to repeat its one
    /// bar until the operator moves on. Off by default.
    pub fn set_infinite_at_start(&mut self, on: bool) {
        self.infinite_at_start = on;
        self.infinite = on;
    }

    /// Sets every stem's effective gain and bus at once, with no ramp, in
    /// the order of the sources the player was built with (the song's stem
    /// order, as [`domain::effective_gains`] gives them): for a player that
    /// is not playing yet, before it is handed to the audio thread. Extra
    /// entries are ignored; stems without one keep theirs.
    pub fn set_stem_gains(&mut self, gains: &[StemGain]) {
        for (stem, gain) in self.stems.iter_mut().zip(gains) {
            stem.live = Ramp::at(Gain::new(gain.gain).get());
            stem.place(gain.bus);
        }
    }

    /// Sets the generated click's gain at once ([`Command::SetClickGain`]
    /// without the ramp), for a player not playing yet.
    pub fn set_click_gain(&mut self, gain: f32) {
        self.click_level = Ramp::at(Gain::new(gain).get());
    }

    /// Sets the routing at once ([`Command::SetRouting`] without the
    /// crossfade), for a player not playing yet.
    pub fn set_routes(&mut self, routes: Routes) {
        self.routes = routes;
        route_matrix(&routes, self.channels, &mut self.route_to);
        self.route_from.copy_from_slice(&self.route_to);
        self.route_fade = Ramp::at(1.0);
    }

    /// The output channels [`Self::process`] fills.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Applies `command` at once, as if it had come through the queue: for
    /// the thread that owns the player, such as a [`super::Deck`] driving
    /// it. It is not counted by [`Self::applied_commands`]. Real-time safe.
    pub(crate) fn command(&mut self, command: Command) {
        self.apply(command);
    }

    /// Output frames the arrangement plays before it ends if no other
    /// command arrives: the rest of the current position and the positions
    /// after it, or, with a jump or loop queued, the frames to its boundary
    /// and the positions from its target on. `None` while the infinite loop
    /// is on. An estimate when a queued jump is deferred: the arrangement
    /// then plays on. Real-time safe.
    pub(crate) fn frames_to_end(&self) -> Option<u64> {
        if self.infinite {
            return None;
        }
        Some(match self.queued {
            Some(queued) => {
                (self.jump_frame(queued) - self.output_frame) + self.suffix[queued.target]
            }
            None => {
                let entry = self.entries[self.voice.position];
                (entry.len - self.voice.offset) + self.suffix[self.voice.position + 1]
            }
        })
    }

    /// The earliest output frame, from now, where the arrangement can end
    /// if no other command arrives: where [`Self::frames_to_end`] says, or,
    /// when a queued jump or loop is deferred for its audio, where the
    /// arrangement ends without it. A jump to another position that
    /// switches the infinite loop off can end it too. `None` when nothing
    /// can end it. Real-time safe.
    pub(crate) fn earliest_end(&self) -> Option<u64> {
        let entry = self.entries[self.voice.position];
        let natural = (!self.infinite)
            .then(|| (entry.len - self.voice.offset) + self.suffix[self.voice.position + 1]);
        let jumped = self.queued.and_then(|queued| {
            let leaves_loop = queued.kind == JumpKind::Jump && queued.target != self.voice.position;
            (!self.infinite || leaves_loop)
                .then(|| (self.jump_frame(queued) - self.output_frame) + self.suffix[queued.target])
        });
        match (natural, jumped) {
            (Some(natural), Some(jumped)) => Some(natural.min(jumped)),
            (natural, jumped) => natural.or(jumped),
        }
    }

    /// Times the arrangement played to its end while playing and returned
    /// to the start there. An end reached while a pause, stop or panic
    /// ramps out does not count: the operator stopped the item.
    pub(crate) fn ends(&self) -> u64 {
        self.ends
    }

    /// The transport state.
    pub(crate) fn transport(&self) -> Transport {
        self.transport
    }

    /// Output frames played since the last start.
    pub(crate) fn output_frame(&self) -> u64 {
        self.output_frame
    }

    /// How many commands the player has taken from its [`Controller`]'s
    /// queue and applied so far. A [`Status`] read after a call that returns
    /// as many as the controller sent describes the state those commands
    /// left, such as the playhead after [`Command::BackToStart`]. Real-time
    /// safe.
    pub fn applied_commands(&self) -> u64 {
        self.applied
    }

    /// Whether every source can start playing from the playhead now without
    /// running dry: [`StemSource::is_ready`] for the rest of the current
    /// position, the question a queued jump asks for its target. Real-time
    /// safe; a streaming source also takes in the blocks that arrived. The
    /// control side asks it (through whatever thread owns the player) while
    /// the transport is stopped or paused, so a start from a cold stream
    /// does not play a silent first buffer.
    pub fn playhead_ready(&mut self) -> bool {
        let at = self.source_frame();
        let entry = self.entries[self.voice.position];
        let frames = entry.len - self.voice.offset;
        self.stems
            .iter_mut()
            .all(|stem| stem.source.is_ready(at, frames))
    }

    fn apply(&mut self, command: Command) {
        use Transport::*;
        match command {
            Command::Play => match self.transport {
                Stopped => self.transport = Playing,
                Pausing | Paused | Stopping => {
                    self.transport = Playing;
                    self.transport_gain.retarget(1.0, DECLICK_FRAMES);
                }
                Playing | Panicking => {}
            },
            Command::Pause => {
                if self.transport == Playing {
                    self.transport = Pausing;
                    self.transport_gain.retarget(0.0, DECLICK_FRAMES);
                }
            }
            Command::Stop => match self.transport {
                Playing | Pausing => {
                    self.transport = Stopping;
                    self.transport_gain.retarget(0.0, DECLICK_FRAMES);
                }
                Paused | Stopped => self.reset(),
                Stopping | Panicking => {}
            },
            Command::BackToStart => match self.transport {
                Playing | Pausing => self.seam_to(0),
                Paused => {
                    self.voice = Voice::START;
                    self.voice_epoch += 1;
                    self.tails.clear();
                    self.active.clear();
                }
                Stopped | Stopping | Panicking => {}
            },
            Command::QueueJump { position } => {
                if position < self.entries.len() {
                    self.queued = Some(Queued::new(position, JumpKind::Jump));
                }
            }
            Command::CancelJump => self.queued = None,
            Command::SetJumpTiming(timing) => self.jump_timing = timing,
            Command::LiveLoop => {
                self.queued = Some(Queued::new(self.voice.position, JumpKind::Loop));
            }
            Command::SetInfinite(on) => self.infinite = on,
            Command::ToggleFade => {
                self.fade_out = !self.fade_out;
                let target = if self.fade_out { 0.0 } else { 1.0 };
                self.fade.retarget(target, self.fade_frames);
            }
            Command::SetFadeDuration { frames } => self.fade_frames = frames.max(DECLICK_FRAMES),
            Command::Panic => match self.transport {
                Playing | Pausing | Stopping => {
                    self.transport = Panicking;
                    self.transport_gain.retarget(0.0, self.panic_frames);
                }
                Paused | Stopped => self.reset(),
                Panicking => {}
            },
            Command::SetClick(on) => {
                self.click_on = on;
                self.click
                    .retarget(if on { 1.0 } else { 0.0 }, DECLICK_FRAMES);
            }
            Command::SetStemGain { stem, gain } => {
                let (audible, frames) = (self.transport.is_running(), self.gain_ramp);
                if let Some(stem) = self.stems.get_mut(stem) {
                    ramp_to(&mut stem.live, gain.get(), audible, frames);
                }
            }
            Command::SetStemBus { stem, bus } => {
                let (audible, frames) = (self.transport.is_running(), self.gain_ramp);
                if let Some(stem) = self.stems.get_mut(stem) {
                    stem.move_to(bus, audible, frames);
                }
            }
            Command::SetClickGain(gain) => {
                let audible = self.transport.is_running();
                ramp_to(&mut self.click_level, gain.get(), audible, self.gain_ramp);
            }
            Command::SetRouting(routes) => self.route(routes),
        }
    }

    /// Renders `out`, splitting it at every boundary inside it.
    fn render(&mut self, out: &mut [f32]) {
        let channels = self.channels;
        let frames = out.len() / channels;
        let mut done = 0;
        loop {
            self.handle_boundaries();
            self.update_end_fade();
            self.update_hints();
            if done == frames || !self.transport.is_running() {
                return;
            }
            let n = ((frames - done) as u64).min(self.frames_to_next_event()) as usize;
            self.render_block(&mut out[channels * done..channels * (done + n)]);
            self.advance(n as u64);
            done += n;
        }
    }

    /// Executes everything due at the current output frame.
    fn handle_boundaries(&mut self) {
        loop {
            match self.transport {
                Transport::Stopped | Transport::Paused => return,
                Transport::Pausing if self.transport_gain.is_done() => {
                    self.transport = Transport::Paused;
                    return;
                }
                Transport::Stopping | Transport::Panicking if self.transport_gain.is_done() => {
                    self.reset();
                    return;
                }
                _ => {}
            }
            if let Some(queued) = self.queued
                && self.jump_frame(queued) == self.output_frame
            {
                if self.target_ready(queued.target) {
                    self.queued = None;
                    if queued.kind == JumpKind::Jump && queued.target != self.voice.position {
                        self.infinite = false;
                    }
                    self.seam_to(queued.target);
                } else {
                    self.queued = Some(Queued {
                        not_before: self.output_frame + 1,
                        deferrals: queued.deferrals + 1,
                        ..queued
                    });
                }
                continue;
            }
            if self.voice.offset == self.entries[self.voice.position].len {
                let Some(next) = self.natural_next() else {
                    // An end reached while a pause, stop or panic ramps
                    // out is the operator's stop, not the item ending.
                    if self.transport == Transport::Playing {
                        self.ends += 1;
                    }
                    self.reset();
                    return;
                };
                // A jump deferred at this boundary whose target the
                // arrangement enters on its own has done its job; executing
                // it later would play the target again.
                if self.queued.is_some_and(|queued| queued.target == next) {
                    self.queued = None;
                }
                self.seam_to(next);
                continue;
            }
            return;
        }
    }

    /// Frames until the next boundary: the end of the current position, the
    /// queued jump, or the end of a stopping ramp. At least 1 after
    /// [`Self::handle_boundaries`].
    fn frames_to_next_event(&self) -> u64 {
        let entry = self.entries[self.voice.position];
        let mut frames = entry.len - self.voice.offset;
        if let Some(queued) = self.queued {
            frames = frames.min(self.jump_frame(queued) - self.output_frame);
        }
        if matches!(
            self.transport,
            Transport::Pausing | Transport::Stopping | Transport::Panicking
        ) {
            frames = frames.min(self.transport_gain.remaining());
        }
        if let Some(due) = self.end_fade_due()
            && due > 0
        {
            frames = frames.min(due);
        }
        frames
    }

    /// Frames until the end fade must start, while it is set and not
    /// running, the transport plays, and the arrangement can end; 0 when
    /// it is due now.
    fn end_fade_due(&self) -> Option<u64> {
        let window = self.end_fade?;
        if self.end_fading || self.transport != Transport::Playing {
            return None;
        }
        Some(self.earliest_end()?.saturating_sub(window))
    }

    /// Keeps the end fade on the arrangement's predicted end (see
    /// [`Self::set_end_fade`]): starts it when it is due, ramping the end
    /// gain from where it stands to silence over exactly the frames left;
    /// speeds it up when the end came closer; and cancels it, ramping the
    /// stems back, when the end left the window or can no longer come.
    fn update_end_fade(&mut self) {
        let Some(window) = self.end_fade else {
            return;
        };
        if self.end_fading {
            match self.earliest_end() {
                Some(left) if left <= window => {
                    if left < self.end_gain.remaining() {
                        self.end_gain.retarget_over(0.0, left);
                    }
                }
                _ => {
                    self.end_fading = false;
                    self.end_gain.retarget(1.0, DECLICK_FRAMES);
                }
            }
            return;
        }
        if self.end_fade_due() == Some(0) {
            let left = self.earliest_end().unwrap_or(0);
            self.end_fading = true;
            self.end_gain.retarget_over(0.0, left);
        }
    }

    /// Output frame of the first boundary, at or after `queued.not_before`,
    /// where `queued` may execute: the end of the current position, or with
    /// [`JumpTiming::NextBar`] the next bar line inside it.
    fn jump_frame(&self, queued: Queued) -> u64 {
        let entry = self.entries[self.voice.position];
        let at_end = self.output_frame + (entry.len - self.voice.offset);
        let earliest = queued.not_before.max(self.output_frame);
        if queued.kind == JumpKind::Loop || self.jump_timing == JumpTiming::SectionEnd {
            // Past `earliest` only for the instant between a deferral at a
            // section end and the seam that follows it.
            return at_end.max(earliest);
        }
        let source = entry.src_start + self.voice.offset;
        let from = source + (earliest - self.output_frame);
        let boundary = match self.grid.next_bar_line(from) {
            Some(bar) if bar < entry.src_start + entry.len => self.output_frame + (bar - source),
            _ => at_end,
        };
        boundary.max(earliest)
    }

    /// Whether every source has the start of position `target` ready.
    fn target_ready(&mut self, target: usize) -> bool {
        let entry = self.entries[target];
        self.stems
            .iter_mut()
            .all(|stem| stem.source.is_ready(entry.src_start, entry.len))
    }

    /// Position the arrangement enters at the end of the current one when no
    /// queued jump executes: the current one again with the infinite loop
    /// on, else the next one; `None` at the end of the arrangement.
    fn natural_next(&self) -> Option<usize> {
        let position = self.voice.position;
        if self.infinite {
            Some(position)
        } else {
            (position + 1 < self.entries.len()).then_some(position + 1)
        }
    }

    /// Source start of `position` when entering it at the end of the current
    /// position jumps in the source; `None` when it follows on contiguously,
    /// where the window ahead of the playhead already covers it.
    fn seam_target(&self, position: usize) -> Option<u64> {
        let current = self.entries[self.voice.position];
        let start = self.entries[position].src_start;
        (start != current.src_start + current.len).then_some(start)
    }

    /// Sends the sources a new prefetch hint when the playhead jumped or the
    /// targets changed: the queued target; both places the end of the
    /// current position can go, the next position and its own start (a
    /// loop), the one the infinite loop selects first; and the start of the
    /// arrangement (for back to start and stop). Hinting both keeps a loop
    /// switched on or off just before the end from meeting audio the source
    /// has not buffered.
    fn update_hints(&mut self) {
        let mut targets = [0; MAX_PREFETCH_TARGETS];
        let mut count = 0;
        let position = self.voice.position;
        let next = (position + 1 < self.entries.len()).then_some(position + 1);
        let (likely, other) = if self.infinite {
            (Some(position), next)
        } else {
            (next, Some(position))
        };
        let candidates = [
            self.queued
                .map(|queued| self.entries[queued.target].src_start),
            likely.and_then(|position| self.seam_target(position)),
            other.and_then(|position| self.seam_target(position)),
            Some(self.entries[0].src_start),
        ];
        for target in candidates.into_iter().flatten() {
            if !targets[..count].contains(&target) {
                targets[count] = target;
                count += 1;
            }
        }
        let hint = Hint {
            voice_epoch: self.voice_epoch,
            targets,
            count,
        };
        if self.last_hint == Some(hint) {
            return;
        }
        self.last_hint = Some(hint);
        let playhead = self.source_frame();
        for stem in &mut self.stems {
            stem.source.prefetch(playhead, &targets[..count]);
        }
    }

    fn source_frame(&self) -> u64 {
        self.entries[self.voice.position].src_start + self.voice.offset
    }

    /// Leaves the current position for the start of `target`. When the
    /// source jumps, the outgoing side keeps playing as a tail and the
    /// incoming side fades in, as [`seam_fade`] says. When it does not, the
    /// same audio plays on, a fade-in still running included.
    fn seam_to(&mut self, target: usize) {
        let outgoing_end = self.source_frame();
        let incoming = self.entries[target];
        self.voice_epoch += 1;
        if outgoing_end == incoming.src_start {
            self.voice = Voice {
                position: target,
                offset: 0,
                env_start: self.voice.envelope_position(),
                ..self.voice
            };
            return;
        }
        let fade = seam_fade(
            self.seam_crossfade,
            outgoing_end,
            incoming.src_start,
            incoming.len,
            self.song_length,
        );
        if fade > 0 {
            if self.tails.len() == MAX_TAILS {
                self.tails.remove(0);
            }
            self.tails.push(Tail {
                src_pos: outgoing_end,
                len: fade,
                played: 0,
                fade_in: self.voice.fade_in,
                cut_offset: self.voice.envelope_position(),
            });
        }
        self.voice = Voice {
            position: target,
            offset: 0,
            fade_in: fade,
            env_start: 0,
        };
    }

    /// Back to the start, stopped.
    fn reset(&mut self) {
        self.transport = Transport::Stopped;
        self.transport_gain = Ramp::at(1.0);
        self.output_frame = 0;
        self.voice = Voice::START;
        self.voice_epoch += 1;
        self.tails.clear();
        self.active.clear();
        self.queued = None;
        self.infinite = self.infinite_at_start;
        self.fade = Ramp::at(1.0);
        self.fade_out = false;
        self.end_gain = Ramp::at(1.0);
        self.end_fading = false;
        // Nothing is heard now: gain, bus and routing changes under way land.
        for stem in &mut self.stems {
            stem.live.finish();
            stem.place(stem.bus);
        }
        self.click_level.finish();
        self.route_from.copy_from_slice(&self.route_to);
        self.route_fade = Ramp::at(1.0);
    }

    /// Switches to `routes`: while the transport runs, a crossfade from
    /// the router's gains as heard now (part way through an earlier change,
    /// when one runs) to the new ones; at once otherwise.
    fn route(&mut self, routes: Routes) {
        if routes == self.routes {
            return;
        }
        if !self.transport.is_running() {
            self.set_routes(routes);
            return;
        }
        let reached = self.route_fade.level();
        if self.route_fade.is_done() {
            self.route_from.copy_from_slice(&self.route_to);
        } else {
            for (from, to) in self.route_from.iter_mut().zip(&self.route_to) {
                *from += (to - *from) * reached;
            }
        }
        route_matrix(&routes, self.channels, &mut self.route_to);
        self.route_fade = Ramp::at(0.0);
        self.route_fade.retarget_over(1.0, self.gain_ramp);
        self.routes = routes;
    }

    /// Whether the stems sum straight into outputs 1 and 2, as they did
    /// before routing: every bus there, no master, no routing crossfade.
    fn plain(&self) -> bool {
        self.channels >= 2 && self.routes == Routes::STEREO && self.route_fade.is_done()
    }

    /// Renders the frames of `out` (interleaved channels) with no boundary
    /// inside them.
    fn render_block(&mut self, out: &mut [f32]) {
        let channels = self.channels;
        let n = out.len() / channels;
        for i in 0..n {
            self.track_gain[i] = self.fade.next() * self.end_gain.next();
            self.click_gain[i] = self.click.next() * self.click_level.next();
            self.master[i] = self.master_gain * self.transport_gain.next();
        }
        let stems = self.meters.stems();
        for meters in &mut self.block_meters {
            meters.clear(stems);
        }
        self.meter_segments = self.meters.segments(n);
        if !self.plain() {
            self.render_buses(out, n);
        } else if channels == 2 {
            self.render_stereo(out);
        } else {
            let mut mix = mem::take(&mut self.mix);
            let stereo = &mut mix[..2 * n];
            stereo.fill(0.0);
            self.render_stereo(stereo);
            for (frame, pair) in out.chunks_exact_mut(channels).zip(stereo.chunks_exact(2)) {
                frame[0] = pair[0];
                frame[1] = pair[1];
            }
            self.mix = mix;
        }
        let [before, after] = &self.block_meters;
        self.meters.add(before, after, n as u64);
        self.rendered += n as u64;
    }

    /// The stereo mix of every stem in stem order, then the click, then the
    /// master gain, into the stereo buffer `out`: exactly what the player
    /// played before routing.
    fn render_stereo(&mut self, out: &mut [f32]) {
        let n = out.len() / 2;
        // Every bus plays on 1/2 here, so a stem moving between buses is
        // not heard moving; its move keeps time.
        for stem in &mut self.stems {
            stem.bus_fade.skip(n as u64);
        }
        let source = self.source_frame();
        let stride = 2 * self.max_frames;
        let mut meter_buses = mem::take(&mut self.meter_buses);
        for bus in 0..BUSES {
            meter_buses[bus * stride..bus * stride + 2 * n].fill(0.0);
        }
        let block = Block {
            n,
            source,
            tails: &self.tails,
            track_gain: &self.track_gain,
            master: &self.master,
            meter_from: self.meter_segments.0,
            meter_split: self.meter_segments.1,
            envelope: Envelope::FadeIn(self.voice.fade_in),
            envelope_start: self.voice.envelope_position(),
        };
        let [before, after] = &mut self.block_meters;
        for (index, stem) in self.stems.iter_mut().enumerate() {
            let bus = stem.bus.index();
            add_stem(
                stem,
                &block,
                &mut self.live,
                &mut self.read,
                out,
                &mut self.underruns,
                StemMeter {
                    peaks: before.stems.get_mut(index).zip(after.stems.get_mut(index)),
                    bus: Some(&mut meter_buses[bus * stride..bus * stride + 2 * n]),
                    heard: &mut self.meter_heard,
                },
            );
        }
        let click = Bus::ClickGuide.index();
        self.add_clicks(
            out,
            Some(&mut meter_buses[click * stride..click * stride + 2 * n]),
            source,
        );
        for (frame, gain) in out.chunks_exact_mut(2).zip(&self.master) {
            frame[0] *= gain;
            frame[1] *= gain;
        }
        let (from, split) = self.meter_segments;
        for bus in 0..BUSES {
            let signal = &meter_buses[bus * stride..bus * stride + 2 * n];
            let frames = signal.chunks_exact(2).zip(&self.master).enumerate();
            for (i, (frame, gain)) in frames.skip(from) {
                let meters = &mut self.block_meters[usize::from(i >= split)];
                raise(&mut meters.buses[bus], frame[0] * gain, frame[1] * gain);
            }
        }
        for (i, frame) in out.chunks_exact(2).enumerate().skip(from) {
            let meters = &mut self.block_meters[usize::from(i >= split)];
            raise(&mut meters.master, frame[0], frame[1]);
        }
        self.meter_buses = meter_buses;
    }

    /// Each stem into its bus (crossfading between two while it moves), the
    /// click into Click & Guide, the master gain on every bus, then the
    /// router ([`route_matrix`]), its gains crossfading during a change.
    fn render_buses(&mut self, out: &mut [f32], n: usize) {
        let channels = self.channels;
        let stride = 2 * self.max_frames;
        let mut buses = mem::take(&mut self.buses);
        for bus in 0..BUSES {
            buses[bus * stride..bus * stride + 2 * n].fill(0.0);
        }
        let source = self.source_frame();
        let mut used = [false; BUSES];
        let block = Block {
            n,
            source,
            tails: &self.tails,
            track_gain: &self.track_gain,
            master: &self.master,
            meter_from: self.meter_segments.0,
            meter_split: self.meter_segments.1,
            envelope: Envelope::FadeIn(self.voice.fade_in),
            envelope_start: self.voice.envelope_position(),
        };
        let [before, after] = &mut self.block_meters;
        for (index, stem) in self.stems.iter_mut().enumerate() {
            let meter = StemMeter {
                peaks: before.stems.get_mut(index).zip(after.stems.get_mut(index)),
                bus: None,
                heard: &mut self.meter_heard,
            };
            if stem.bus_fade.is_done() {
                let bus = stem.bus.index();
                used[bus] = true;
                let target = &mut buses[bus * stride..bus * stride + 2 * n];
                add_stem(
                    stem,
                    &block,
                    &mut self.live,
                    &mut self.read,
                    target,
                    &mut self.underruns,
                    meter,
                );
                continue;
            }
            // Moving between buses: into each in proportion, frame by frame.
            let heard = &mut self.stem_mix[..2 * n];
            heard.fill(0.0);
            add_stem(
                stem,
                &block,
                &mut self.live,
                &mut self.read,
                heard,
                &mut self.underruns,
                meter,
            );
            for (i, frame) in heard.chunks_exact(2).enumerate() {
                let reached = stem.bus_fade.next();
                for bus in 0..BUSES {
                    let (from, to) = (stem.bus_from[bus], stem.bus_to[bus]);
                    if from == 0.0 && to == 0.0 {
                        continue;
                    }
                    let weight = from + (to - from) * reached;
                    buses[bus * stride + 2 * i] += frame[0] * weight;
                    buses[bus * stride + 2 * i + 1] += frame[1] * weight;
                    used[bus] = true;
                }
            }
        }
        if self.bursts.is_some() {
            let click = Bus::ClickGuide.index();
            used[click] = true;
            self.add_clicks(
                &mut buses[click * stride..click * stride + 2 * n],
                None,
                source,
            );
        }
        for bus in (0..BUSES).filter(|&bus| used[bus]) {
            let signal = &mut buses[bus * stride..bus * stride + 2 * n];
            for (frame, gain) in signal.chunks_exact_mut(2).zip(&self.master) {
                frame[0] *= gain;
                frame[1] *= gain;
            }
            let (from, split) = self.meter_segments;
            for (i, frame) in signal.chunks_exact(2).enumerate().skip(from) {
                let meters = &mut self.block_meters[usize::from(i >= split)];
                raise(&mut meters.buses[bus], frame[0], frame[1]);
            }
        }
        let (from, split) = self.meter_segments;
        for i in from..n {
            let (mut left, mut right) = (0.0, 0.0);
            for bus in (0..BUSES).filter(|&bus| used[bus]) {
                left += buses[bus * stride + 2 * i];
                right += buses[bus * stride + 2 * i + 1];
            }
            let meters = &mut self.block_meters[usize::from(i >= split)];
            raise(&mut meters.master, left, right);
        }

        let moving = !self.route_fade.is_done();
        if moving {
            for value in &mut self.route_gain[..n] {
                *value = self.route_fade.next();
            }
        }
        let reached = &self.route_gain[..n];
        for bus in (0..BUSES).filter(|&bus| used[bus]) {
            let signal = &buses[bus * stride..bus * stride + 2 * n];
            for side in 0..2 {
                for c in 0..channels {
                    let k = (bus * 2 + side) * channels + c;
                    let (from, to) = (self.route_from[k], self.route_to[k]);
                    if moving && (from != 0.0 || to != 0.0) {
                        for i in 0..n {
                            let weight = from + (to - from) * reached[i];
                            out[i * channels + c] += signal[2 * i + side] * weight;
                        }
                    } else if !moving && to != 0.0 {
                        for i in 0..n {
                            out[i * channels + c] += signal[2 * i + side] * to;
                        }
                    }
                }
            }
        }
        self.buses = buses;
    }

    /// Adds the bursts still sounding, then the bursts of the ticks in the
    /// source range being played, in the order they start; to `meter` too,
    /// the Click & Guide bus as the meters read it, when given.
    fn add_clicks(&mut self, out: &mut [f32], mut meter: Option<&mut [f32]>, source: u64) {
        let Some(bursts) = &self.bursts else {
            return;
        };
        let n = out.len() / 2;
        let click_gain = &self.click_gain[..n];
        for burst in &mut self.active {
            let table = &bursts.table(burst.level)[burst.offset..];
            let k = n.min(table.len());
            add_burst(&mut out[..2 * k], &table[..k], burst.gain, &click_gain[..k]);
            if let Some(meter) = meter.as_deref_mut() {
                add_burst(
                    &mut meter[..2 * k],
                    &table[..k],
                    burst.gain,
                    &click_gain[..k],
                );
            }
            burst.offset += k;
        }
        self.active
            .retain(|burst| burst.offset < bursts.table(burst.level).len());
        for click in self.grid.clicks_in(source..source + n as u64) {
            let gain = self.click_levels[level_index(click.level)];
            if gain == 0.0 {
                continue;
            }
            let start = (click.at - source) as usize;
            let table = bursts.table(click.level);
            let k = (n - start).min(table.len());
            add_burst(
                &mut out[2 * start..2 * (start + k)],
                &table[..k],
                gain,
                &click_gain[start..start + k],
            );
            if let Some(meter) = meter.as_deref_mut() {
                add_burst(
                    &mut meter[2 * start..2 * (start + k)],
                    &table[..k],
                    gain,
                    &click_gain[start..start + k],
                );
            }
            if k < table.len() && self.active.len() < self.max_bursts {
                self.active.push(ActiveBurst {
                    level: click.level,
                    gain,
                    offset: k,
                });
            }
        }
    }

    /// Moves every counter `n` frames on.
    fn advance(&mut self, n: u64) {
        self.voice.offset += n;
        self.output_frame += n;
        for tail in &mut self.tails {
            let k = n.min(tail.remaining());
            tail.played += k;
            tail.src_pos += k;
        }
        self.tails.retain(|tail| tail.remaining() > 0);
    }
}

/// Adds `table` scaled by `gain` and the per-frame click gain to both
/// channels of `out`.
fn add_burst(out: &mut [f32], table: &[f32], gain: f32, click_gain: &[f32]) {
    for ((frame, sample), click) in out.chunks_exact_mut(2).zip(table).zip(click_gain) {
        let value = sample * (gain * click);
        frame[0] += value;
        frame[1] += value;
    }
}

/// Moves `ramp` to `to`: over `frames` frames while the transport is
/// `audible`, at once otherwise.
fn ramp_to(ramp: &mut Ramp, to: f32, audible: bool, frames: u64) {
    if audible {
        ramp.retarget_over(to, frames);
    } else {
        *ramp = Ramp::at(to);
    }
}
