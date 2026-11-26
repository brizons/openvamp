//! Two players on one audio thread: a setlist's current item and the next
//! one (S3); see [`Deck`].

use std::fmt;
use std::mem;

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use super::{COMMAND_CAPACITY, Command, Player, SendError, Status, Transport};

/// Players the deck keeps ramping out at once after selections; past that
/// the oldest is cut short.
const MAX_LEAVING: usize = 4;

/// Players the deck holds for the control thread to take back. Should both
/// the queue and the deck's own list ever be full, a player is leaked
/// rather than freed on the audio thread.
const RETURN_CAPACITY: usize = 16;

/// What the deck does when its current item plays to its end, with a cued
/// item waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Handoff {
    /// The current item stops at its start; the cued one waits.
    #[default]
    Stop,
    /// The cued item starts on the frame after the current item's last.
    AutoLink,
    /// The cued item starts `frames` before the current item's end (or at
    /// once, when less is left) and the two blend linearly until that end.
    Crossfade {
        /// Length of the blend in output frames.
        frames: u64,
    },
}

/// A crossfade under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendStatus {
    /// Item the blend fades out (the one it fades in is current).
    pub from: u64,
    /// Length of the blend in frames.
    pub frames: u64,
    /// Frames of the blend played so far.
    pub played: u64,
}

/// A snapshot of the deck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeckStatus {
    /// The current item's player.
    pub current: Status,
    /// The current item, as the control thread named it.
    pub item: u64,
    /// The cued item, if any.
    pub cued: Option<u64>,
    /// The crossfade under way, if any.
    pub blend: Option<BlendStatus>,
    /// Items still heard while they ramp out after a selection.
    pub leaving: usize,
    /// Times an item played to its end and stopped there with no handoff.
    pub finished: u64,
    /// The item that did so last.
    pub finished_item: Option<u64>,
}

/// The deck's message queue is full; the message was not sent (a player
/// it carried is dropped on the calling thread).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckFull;

impl fmt::Display for DeckFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the audio thread has not taken the last messages yet")
    }
}

impl std::error::Error for DeckFull {}

/// Why the deck refused a player ([`DeckController::cue`],
/// [`DeckController::select`]); the player is dropped on the calling thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckError {
    /// The message queue is full ([`DeckFull`]).
    Full,
    /// The player renders another number of output channels than the deck:
    /// every player of a deck renders the same.
    ChannelMismatch {
        /// The deck's output channels.
        deck: usize,
        /// The player's.
        player: usize,
    },
}

impl fmt::Display for DeckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => DeckFull.fmt(f),
            Self::ChannelMismatch { deck, player } => write!(
                f,
                "the player renders {player} output channels and the deck {deck}; every player of a deck renders the same"
            ),
        }
    }
}

impl std::error::Error for DeckError {}

impl From<DeckFull> for DeckError {
    fn from(_: DeckFull) -> Self {
        Self::Full
    }
}

enum Message {
    Command(Command),
    /// For the item with this number wherever the deck holds it; for none
    /// when `None`.
    ItemCommand {
        item: Option<u64>,
        command: Command,
    },
    Cue {
        player: Box<Player>,
        item: u64,
        handoff: Handoff,
    },
    Uncue,
    Select {
        player: Box<Player>,
        item: u64,
    },
    SelectCued,
}

struct Cued {
    player: Box<Player>,
    item: u64,
    handoff: Handoff,
}

/// How a leaving item is heard.
#[derive(Debug, Clone, Copy)]
enum Exit {
    /// Blending out under the current item.
    Blend,
    /// At a fixed gain, ramping out on its own stop.
    Fixed(f32),
}

struct Leaving {
    player: Box<Player>,
    item: u64,
    exit: Exit,
}

#[derive(Debug, Clone, Copy)]
struct Blend {
    from: u64,
    len: u64,
    played: u64,
}

impl Blend {
    /// Gain of the incoming item at blend frame `t`; the outgoing item gets
    /// the rest. Sampled mid-frame like the seam envelopes.
    fn gain(&self, t: u64) -> f64 {
        (t.min(self.len - 1) as f64 + 0.5) / self.len as f64
    }
}

/// Plays a setlist's current item and hands over to the cued one.
///
/// A [`Deck`] plays its *current* [`Player`] and can hold a second, *cued*
/// one, prepared on the control thread with its start already buffered.
/// What happens when the current item plays to its end is the cue's
/// [`Handoff`]:
///
/// - [`Handoff::AutoLink`]: the cued item starts on the frame right after
///   the current item's last frame. The deck splits the buffer wherever the
///   end can come (a queued jump or loop executing or deferred, a blend
///   under way), so the switch is sample-exact whatever the buffer size.
/// - [`Handoff::Crossfade`]: the cued item starts that many frames before
///   the current item's end and the two blend linearly, the gains summing to
///   1 on every frame, so two items that peak at −1 dBFS never sum above it
///   (an equal-power blend would). The blend clock follows the incoming
///   item's playhead: a pause holds it, and transport commands (play,
///   pause, stop, panic) reach both items while they blend. The operator's
///   settings (jump timing, fade duration, click) reach the cued item as
///   well; every other command reaches the current item only.
/// - [`Handoff::Stop`]: nothing; the item stops at its start as a player
///   does at its end, and [`DeckStatus::finished`] counts it, so the
///   control thread can apply a setlist's Cue next or Stay.
///
/// Only an end reached while the item plays hands over or counts as
/// finished: when a pause, stop or panic ramps out past the end, the item
/// just stops at its start and the cued item waits.
///
/// The control thread can also make an item current at once
/// ([`DeckController::select`], [`DeckController::select_cued`]): the
/// current item leaves with its stop ramp, still heard until it is silent,
/// and the new one waits stopped at its start.
///
/// Real-time rules: every message travels through a lock-free queue, and a
/// player crosses it in a box, so taking one in moves a pointer. A player
/// the deck no longer needs (ended, uncued, or silent after leaving) goes
/// back to the control thread through a second queue
/// ([`DeckController::returned`]) and is dropped there, never on the audio
/// thread: dropping a streamed player joins its disk thread. All buffers
/// are sized when the deck is built.
pub struct Deck {
    current: Box<Player>,
    item: u64,
    cued: Option<Cued>,
    leaving: Vec<Leaving>,
    blend: Option<Blend>,
    messages: Consumer<Message>,
    returns: Producer<Box<Player>>,
    /// Boxed as they travel: handing one to the queue moves a pointer.
    #[allow(clippy::vec_box)]
    retired: Vec<Box<Player>>,
    scratch: Vec<f32>,
    max_frames: usize,
    /// Output channels of every player: the current one's.
    channels: usize,
    applied: u64,
    finished: u64,
    finished_item: Option<u64>,
}

/// Sends messages to its [`Deck`] from the control thread and takes back
/// the players the deck no longer needs.
pub struct DeckController {
    messages: Producer<Message>,
    returns: Consumer<Box<Player>>,
    /// The output channels every player of the deck renders.
    channels: usize,
    /// The item cued last, until it is sent back or selected: the one
    /// [`Self::send_cued`] is for, wherever the deck holds it by then.
    cued: Option<u64>,
}

impl Deck {
    /// A deck playing `player` as item `item` (a number the control thread
    /// chooses to tell items apart), and the [`DeckController`] that drives
    /// it. `max_frames` is the players' largest buffer; the deck mixes
    /// leaving items in pieces of that size.
    pub fn new(player: Player, item: u64, max_frames: usize) -> (Self, DeckController) {
        let max_frames = max_frames.max(1);
        let channels = player.channels();
        let (producer, messages) = RingBuffer::new(COMMAND_CAPACITY);
        let (returns, back) = RingBuffer::new(RETURN_CAPACITY);
        let deck = Self {
            current: Box::new(player),
            item,
            cued: None,
            leaving: Vec::with_capacity(MAX_LEAVING),
            blend: None,
            messages,
            returns,
            retired: Vec::with_capacity(RETURN_CAPACITY),
            scratch: vec![0.0; channels * max_frames],
            max_frames,
            channels,
            applied: 0,
            finished: 0,
            finished_item: None,
        };
        let controller = DeckController {
            messages: producer,
            returns: back,
            channels,
            cued: None,
        };
        (deck, controller)
    }

    /// Applies every pending message, then fills `out` with interleaved
    /// stereo frames (a trailing odd sample is left at 0.0). With only a
    /// current item it plays exactly what that player plays. Real-time
    /// safe: no allocation, no lock, no I/O.
    pub fn process(&mut self, out: &mut [f32]) {
        self.take_messages();
        out.fill(0.0);
        let frames = out.len() / self.channels;
        if frames == 0 {
            // Lets the player take in what the messages changed.
            self.current.process(&mut []);
        }
        let mut done = 0;
        while done < frames {
            let n = self.piece(frames - done);
            let channels = self.channels;
            self.render_piece(&mut out[channels * done..channels * (done + n)]);
            done += n;
        }
        self.settle();
    }

    /// A snapshot of the deck.
    pub fn status(&self) -> DeckStatus {
        DeckStatus {
            current: self.current.status(),
            item: self.item,
            cued: self.cued.as_ref().map(|cued| cued.item),
            blend: self.blend.map(|blend| BlendStatus {
                from: blend.from,
                frames: blend.len,
                played: blend.played,
            }),
            leaving: self.leaving.len(),
            finished: self.finished,
            finished_item: self.finished_item,
        }
    }

    /// [`Player::playhead_ready`] for the current item. Real-time safe.
    pub fn playhead_ready(&mut self) -> bool {
        self.current.playhead_ready()
    }

    /// How many messages the deck has taken from its controller's queue and
    /// applied: a [`Self::status`] read after a call that returns as many as
    /// were sent describes the state they left.
    pub fn applied_messages(&self) -> u64 {
        self.applied
    }

    fn take_messages(&mut self) {
        while let Ok(message) = self.messages.pop() {
            self.applied += 1;
            match message {
                Message::Command(command) => self.apply(command),
                Message::ItemCommand {
                    item: Some(item),
                    command,
                } => self.apply_to(item, command),
                Message::ItemCommand { item: None, .. } => {}
                Message::Cue {
                    player,
                    item,
                    handoff,
                } => {
                    let cued = Cued {
                        player,
                        item,
                        handoff,
                    };
                    if let Some(old) = self.cued.replace(cued) {
                        self.retire(old.player);
                    }
                }
                Message::Uncue => {
                    if let Some(old) = self.cued.take() {
                        self.retire(old.player);
                    }
                }
                Message::Select { player, item } => self.select(player, item),
                Message::SelectCued => {
                    if let Some(cued) = self.cued.take() {
                        self.select(cued.player, cued.item);
                    }
                }
            }
        }
    }

    /// Transport commands reach both items of a blend, and the operator's
    /// settings (jump timing, fade duration, click) the cued item too, so it
    /// starts the way the operator works; the others reach only the current
    /// item, since its positions and loops are its own.
    fn apply(&mut self, command: Command) {
        self.current.command(command);
        match command {
            Command::Play | Command::Pause | Command::Stop | Command::Panic => {
                for leaving in &mut self.leaving {
                    if let Exit::Blend = leaving.exit {
                        leaving.player.command(command);
                    }
                }
            }
            Command::SetJumpTiming(_) | Command::SetFadeDuration { .. } | Command::SetClick(_) => {
                if let Some(cued) = &mut self.cued {
                    cued.player.command(command);
                }
            }
            // The device's routing and the Click & Guide bus are global:
            // every item heard, and the cued one, follow them.
            Command::SetRouting(_) | Command::SetClickGain(_) => {
                if let Some(cued) = &mut self.cued {
                    cued.player.command(command);
                }
                for leaving in &mut self.leaving {
                    leaving.player.command(command);
                }
            }
            _ => {}
        }
    }

    /// Applies `command` to item `item`, current, cued or leaving; to none
    /// when the deck no longer holds it.
    fn apply_to(&mut self, item: u64, command: Command) {
        if self.item == item {
            self.current.command(command);
        } else if let Some(cued) = &mut self.cued
            && cued.item == item
        {
            cued.player.command(command);
        } else if let Some(leaving) = self.leaving.iter_mut().find(|leaving| leaving.item == item) {
            leaving.player.command(command);
        }
    }

    /// Makes `player` the current item, stopped at its start. The items
    /// heard so far leave with their stop ramp at the level they had.
    fn select(&mut self, player: Box<Player>, item: u64) {
        let gain_in = match self.blend.take() {
            Some(blend) => {
                let gain = blend.gain(blend.played);
                for leaving in &mut self.leaving {
                    if let Exit::Blend = leaving.exit {
                        leaving.exit = Exit::Fixed((1.0 - gain) as f32);
                        leaving.player.command(Command::Stop);
                    }
                }
                gain as f32
            }
            None => 1.0,
        };
        let mut old = mem::replace(&mut self.current, player);
        let old_item = mem::replace(&mut self.item, item);
        if old.transport() == Transport::Stopped {
            self.retire(old);
            return;
        }
        old.command(Command::Stop);
        self.leave(old, old_item, Exit::Fixed(gain_in));
    }

    fn leave(&mut self, player: Box<Player>, item: u64, exit: Exit) {
        if self.leaving.len() == MAX_LEAVING {
            let oldest = self.leaving.remove(0);
            self.retire(oldest.player);
        }
        self.leaving.push(Leaving { player, item, exit });
    }

    /// Frames to render before the next event: a blend starting or
    /// ending, the current item's end under an AutoLink, or the scratch
    /// size while other items are heard. Starts a crossfade that is due.
    fn piece(&mut self, left: usize) -> usize {
        self.start_blend();
        let mut n = left as u64;
        if self.blend.is_some() || !self.leaving.is_empty() {
            n = n.min(self.max_frames as u64);
        }
        if let Some(blend) = self.blend {
            n = n.min(blend.len - blend.played);
        }
        if let Some(cued) = &self.cued
            && self.current.transport() == Transport::Playing
        {
            match cued.handoff {
                // Wherever the end can come, so an AutoLink lands on the
                // next frame even inside a blend or after a deferred jump.
                Handoff::AutoLink => {
                    if let Some(end) = self.current.earliest_end() {
                        n = n.min(end);
                    }
                }
                Handoff::Crossfade { frames } if self.blend.is_none() => {
                    if let Some(to_end) = self.current.frames_to_end()
                        && to_end > frames
                    {
                        n = n.min(to_end - frames);
                    }
                }
                _ => {}
            }
        }
        n.max(1) as usize
    }

    /// Starts the cued crossfade once the current item has its length or
    /// less left to play.
    fn start_blend(&mut self) {
        let Some(Cued {
            handoff: Handoff::Crossfade { frames },
            ..
        }) = self.cued
        else {
            return;
        };
        if self.blend.is_some() || self.current.transport() != Transport::Playing {
            return;
        }
        let Some(to_end) = self.current.frames_to_end() else {
            return;
        };
        if to_end > frames {
            return;
        }
        let Some(cued) = self.cued.take() else {
            return;
        };
        let outgoing = self.start_cued(cued);
        self.blend = Some(Blend {
            from: outgoing.1,
            len: to_end.max(1),
            played: 0,
        });
        self.leave(outgoing.0, outgoing.1, Exit::Blend);
    }

    /// Makes the cued item current and playing; returns the item it
    /// replaced.
    fn start_cued(&mut self, cued: Cued) -> (Box<Player>, u64) {
        let mut incoming = cued.player;
        incoming.command(Command::Play);
        let outgoing = mem::replace(&mut self.current, incoming);
        let from = mem::replace(&mut self.item, cued.item);
        (outgoing, from)
    }

    /// Renders one piece: the current item, under the blend's gain while one
    /// runs, plus every leaving item; then hands over at the current item's
    /// end and moves the blend on.
    fn render_piece(&mut self, out: &mut [f32]) {
        let channels = self.channels;
        let n = out.len() / channels;
        let before = self.current.output_frame();
        let ends = self.current.ends();
        // Commands apply before the first piece, so a transport that plays
        // at the start of a piece played up to any end inside it.
        let playing = self.current.transport() == Transport::Playing;
        self.current.process(out);
        let after = self.current.output_frame();
        // The incoming item advances over the first frames of the piece
        // only: commands apply at its start, and once a pause or stop has
        // ramped out it stays silent.
        let advanced = if after >= before {
            after - before
        } else {
            n as u64
        };
        let blend = self.blend;
        let t = |i: usize| blend.map_or(0, |blend| blend.played + (i as u64).min(advanced));
        if let Some(blend) = blend {
            for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
                let gain = blend.gain(t(i)) as f32;
                for sample in frame {
                    *sample *= gain;
                }
            }
        }
        for leaving in &mut self.leaving {
            let scratch = &mut self.scratch[..channels * n];
            leaving.player.process(scratch);
            for (i, (frame, heard)) in out
                .chunks_exact_mut(channels)
                .zip(scratch.chunks_exact(channels))
                .enumerate()
            {
                let gain = match (leaving.exit, blend) {
                    (Exit::Fixed(gain), _) => gain,
                    (Exit::Blend, Some(blend)) => (1.0 - blend.gain(t(i))) as f32,
                    (Exit::Blend, None) => 0.0,
                };
                for (sample, heard) in frame.iter_mut().zip(heard) {
                    *sample += heard * gain;
                }
            }
        }

        if let Some(blend) = &mut self.blend {
            let stopped = after < before || self.current.transport() == Transport::Stopped;
            blend.played = (blend.played + advanced).min(blend.len);
            if stopped || blend.played == blend.len {
                self.end_blend();
            }
        }
        if playing && self.current.ends() > ends {
            self.handoff_at_end();
        }
    }

    /// The blend is over. When it ran its full length, the item it faded
    /// out is down to half a frame's step of gain and goes at once; when the
    /// incoming item stopped first, the outgoing one leaves with its stop
    /// ramp at the level it had reached.
    fn end_blend(&mut self) {
        let Some(blend) = self.blend.take() else {
            return;
        };
        let complete = blend.played == blend.len;
        let gain = (1.0 - blend.gain(blend.played)) as f32;
        let mut index = 0;
        while index < self.leaving.len() {
            if let Exit::Blend = self.leaving[index].exit {
                if complete {
                    let leaving = self.leaving.remove(index);
                    self.retire(leaving.player);
                    continue;
                }
                let leaving = &mut self.leaving[index];
                leaving.exit = Exit::Fixed(gain);
                leaving.player.command(Command::Stop);
            }
            index += 1;
        }
    }

    /// The current item played to its end in this piece: an AutoLink cue
    /// starts now, on the next frame; otherwise the item stays stopped.
    fn handoff_at_end(&mut self) {
        let autolink = self.blend.is_none()
            && matches!(
                self.cued,
                Some(Cued {
                    handoff: Handoff::AutoLink,
                    ..
                })
            );
        if autolink && let Some(cued) = self.cued.take() {
            let (ended, _) = self.start_cued(cued);
            self.retire(ended);
            return;
        }
        self.finished += 1;
        self.finished_item = Some(self.item);
    }

    /// Retires the leaving items that went silent and hands retired players
    /// back to the control thread.
    fn settle(&mut self) {
        let mut index = 0;
        while index < self.leaving.len() {
            if self.leaving[index].player.transport() == Transport::Stopped {
                let leaving = self.leaving.remove(index);
                self.retire(leaving.player);
            } else {
                index += 1;
            }
        }
        while let Some(player) = self.retired.pop() {
            if let Err(PushError::Full(player)) = self.returns.push(player) {
                self.retired.push(player);
                break;
            }
        }
    }

    /// Keeps `player` for the control thread to drop.
    fn retire(&mut self, player: Box<Player>) {
        if self.retired.len() < self.retired.capacity() {
            self.retired.push(player);
        } else {
            // Freeing it here would deallocate on the audio thread.
            mem::forget(player);
        }
    }
}

impl DeckController {
    /// Queues `command` for the current item (and, for a transport command,
    /// for both items of a blend).
    pub fn send(&mut self, command: Command) -> Result<(), SendError> {
        self.messages
            .push(Message::Command(command))
            .map_err(|_| SendError::Full(command))
    }

    /// Cues `player` as item `item`, to follow the current item with
    /// `handoff`; a cued item already there comes back. The player should
    /// have the audio at its start buffered ([`Player::playhead_ready`]),
    /// since the deck starts it without waiting.
    /// A player rendering another channel count than the deck is refused
    /// ([`DeckError::ChannelMismatch`]).
    pub fn cue(&mut self, player: Player, item: u64, handoff: Handoff) -> Result<(), DeckError> {
        self.check_channels(&player)?;
        self.push(Message::Cue {
            player: Box::new(player),
            item,
            handoff,
        })?;
        self.cued = Some(item);
        Ok(())
    }

    /// Queues `command` for the item cued last, such as its stem gains
    /// ([`Command::SetStemGain`]) after the bus or master volumes changed,
    /// since its stems are not the current item's: [`Self::send_to`] that
    /// item, so it reaches it even when an AutoLink or Crossfade has made
    /// it current by the time the deck takes the command. It is dropped
    /// when no item is cued: none was, or it was sent back
    /// ([`Self::uncue`]) or selected ([`Self::select_cued`]).
    pub fn send_cued(&mut self, command: Command) -> Result<(), SendError> {
        self.messages
            .push(Message::ItemCommand {
                item: self.cued,
                command,
            })
            .map_err(|_| SendError::Full(command))
    }

    /// Queues `command` for item `item` wherever the deck holds it when it
    /// takes the command: current, cued, or leaving under another item. It
    /// is dropped when the deck no longer holds that item. A command meant
    /// for one item's stems ([`Command::SetStemGain`],
    /// [`Command::SetStemBus`]) goes this way, so a handoff the control
    /// thread has not seen yet cannot give it to another item's stems.
    pub fn send_to(&mut self, item: u64, command: Command) -> Result<(), SendError> {
        self.messages
            .push(Message::ItemCommand {
                item: Some(item),
                command,
            })
            .map_err(|_| SendError::Full(command))
    }

    /// Sends the cued item back.
    pub fn uncue(&mut self) -> Result<(), DeckFull> {
        self.push(Message::Uncue)?;
        self.cued = None;
        Ok(())
    }

    /// Makes `player` the current item as `item`, stopped at its start; the
    /// current item leaves with its stop ramp.
    /// A player rendering another channel count than the deck is refused
    /// ([`DeckError::ChannelMismatch`]).
    pub fn select(&mut self, player: Player, item: u64) -> Result<(), DeckError> {
        self.check_channels(&player)?;
        Ok(self.push(Message::Select {
            player: Box::new(player),
            item,
        })?)
    }

    /// Makes the cued item current, stopped at its start; the current item
    /// leaves with its stop ramp. Nothing happens when no item is cued by
    /// the time the deck takes the message (an AutoLink or Crossfade may
    /// have started it already).
    pub fn select_cued(&mut self) -> Result<(), DeckFull> {
        self.push(Message::SelectCued)?;
        self.cued = None;
        Ok(())
    }

    /// A player the deck no longer needs, to be dropped on this thread.
    pub fn returned(&mut self) -> Option<Player> {
        self.returns.pop().ok().map(|player| *player)
    }

    fn check_channels(&self, player: &Player) -> Result<(), DeckError> {
        if player.channels() == self.channels {
            Ok(())
        } else {
            Err(DeckError::ChannelMismatch {
                deck: self.channels,
                player: player.channels(),
            })
        }
    }

    fn push(&mut self, message: Message) -> Result<(), DeckFull> {
        self.messages.push(message).map_err(|_| DeckFull)
    }
}

impl fmt::Debug for Deck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Deck")
            .field("item", &self.item)
            .field("cued", &self.cued.as_ref().map(|cued| cued.item))
            .field("leaving", &self.leaving.len())
            .field("blend", &self.blend)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for DeckController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeckController").finish_non_exhaustive()
    }
}
