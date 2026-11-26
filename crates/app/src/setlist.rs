//! A setlist playing on one audio device (S3): its items one after another,
//! each with its arrangement, start and end points, fade-out and
//! transition, and click-only tiles (F5, F6.1).

use std::fmt;
use std::mem;
use std::path::PathBuf;

use domain::{
    BusSettings, CROSSFADE_SECONDS, ClickItem, MixFile, Routing, SetlistItem,
    SetlistValidationError, SongItem, SongLookup, Transition,
};
use engine::live::{DeckStatus, Handoff};
use library::{DiskMonitor, Library, SetlistError, SetlistSongs};
use thiserror::Error;

use crate::error::{OpenError, RuntimeError};
use crate::item::{Item, ItemError};
use crate::output::{AudioOutput, OutputCheck};
use crate::session::{Command, PreparedSong, Session, SessionOptions, Snapshot, check_buffer};
use crate::song::SongInfo;

/// The rate the device opens at for a setlist with no playable song: only
/// click-only tiles, which play at any rate.
pub const CLICK_ONLY_SAMPLE_RATE: u32 = 48_000;

/// What a setlist holds and what the library can provide for it, known
/// before anything plays.
#[derive(Debug, Clone, PartialEq)]
pub struct SetlistInfo {
    /// The setlist's name.
    pub name: String,
    /// The rate the device opens at: the first playable song's, or
    /// [`CLICK_ONLY_SAMPLE_RATE`] when only click-only tiles can play.
    pub sample_rate: u32,
    /// Every item, in play order; commands and snapshots index into it.
    pub items: Vec<ItemInfo>,
}

impl SetlistInfo {
    /// Indexes of the items that cannot play, in order.
    pub fn unavailable(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&index| !self.items[index].is_playable())
            .collect()
    }

    /// Indexes of the playable songs whose sample rate is not
    /// [`Self::sample_rate`]: each plays after the device reopens at its
    /// rate, and no AutoLink or Crossfade leads into it.
    pub fn other_rate(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&index| {
                let item = &self.items[index];
                item.is_playable()
                    && item
                        .sample_rate
                        .is_some_and(|rate| rate != self.sample_rate)
            })
            .collect()
    }
}

/// One item of a [`SetlistInfo`].
#[derive(Debug, Clone, PartialEq)]
pub struct ItemInfo {
    /// The item as the setlist stores it.
    pub item: SetlistItem,
    /// The song's sample rate; `None` for a click-only tile, which plays at
    /// the device's rate, and for a song the library cannot provide.
    pub sample_rate: Option<u32>,
    /// Why the item cannot play, every reason; empty when it can.
    pub problems: Vec<String>,
    /// How its AutoLink or Crossfade plays as Cue next when the setlist
    /// plays in order, because the item it leads into runs at another rate
    /// than the device then; `None` when it plays as written.
    pub fallback: Option<RateFallback>,
}

/// An AutoLink or Crossfade that plays as Cue next: the next playable item
/// runs at another rate than the device while this one plays, and nothing
/// resamples, so the device reopens between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateFallback {
    /// Index of the item it leads into.
    pub next: usize,
    /// That item's rate.
    pub next_rate: u32,
    /// The device's rate while this item plays.
    pub device_rate: u32,
}

impl ItemInfo {
    /// The transition the item plays with: Cue next under a
    /// [`Self::fallback`], else its own.
    pub fn plays_as(&self) -> Transition {
        match self.fallback {
            Some(_) => Transition::CueNext,
            None => self.item.transition(),
        }
    }

    /// Whether the item can play.
    pub fn is_playable(&self) -> bool {
        self.problems.is_empty()
    }
}

/// A command a shell sends with [`SetlistSession::send`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetlistCommand {
    /// A command for the current item, as a single-song session takes it:
    /// play, pause, stop, back to start, jumps, loops, fade, panic.
    Item(Command),
    /// Selects the next playable item (F6.1).
    Next,
    /// Selects the previous playable item (F6.1).
    Previous,
    /// Selects the item at this index of [`SetlistInfo::items`]; selecting
    /// the current item stops it at its start.
    Select(usize),
    /// Moves the item at index `from` of [`SetlistInfo::items`] to `to`,
    /// while the setlist plays too: the item playing plays on without a
    /// cut, every index follows its item, and when the item after the
    /// current one changes, the one cued is dropped and the new next item
    /// is prepared and cued as any next item is. Moving an item to its own
    /// place does nothing. The session writes no file: the shell saves the
    /// new order, which [`SetlistSession::info`] holds.
    Move {
        /// Index of the item moved.
        from: usize,
        /// Its index once moved.
        to: usize,
    },
}

/// The state of a setlist session, for a shell to draw;
/// [`SetlistSession::poll`] returns one.
#[derive(Debug, Clone, PartialEq)]
pub struct SetlistSnapshot {
    /// The current item, as a single-song session describes its song
    /// ([`crate::Session::poll`]).
    pub song: Snapshot,
    /// Where the setlist stands.
    pub setlist: SetlistState,
}

/// Where a setlist stands.
#[derive(Debug, Clone, PartialEq)]
pub struct SetlistState {
    /// Index of the current item in [`SetlistInfo::items`]. During a
    /// crossfade it is the item fading in.
    pub current: usize,
    /// Index of the playable item the current one leads to, if any.
    pub next: Option<usize>,
    /// The current item's transition.
    pub transition: Transition,
    /// Whether the next item is prepared and its start buffered, so its
    /// transition will not wait for the disk.
    pub next_ready: bool,
    /// How the current item's AutoLink or Crossfade plays as Cue next, at
    /// the rate the device runs now; `None` when it plays as written.
    pub fallback: Option<RateFallback>,
    /// The crossfade under way, if any.
    pub blend: Option<BlendView>,
    /// The most recent setlist problem; an operator's next, previous or
    /// select clears it.
    pub problem: Option<SetlistProblem>,
}

impl SetlistState {
    /// The transition the current item plays with: Cue next under a
    /// [`Self::fallback`], else [`Self::transition`].
    pub fn plays_as(&self) -> Transition {
        match self.fallback {
            Some(_) => Transition::CueNext,
            None => self.transition,
        }
    }
}

/// A crossfade under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendView {
    /// Index of the item fading out.
    pub from: usize,
    /// Index of the item fading in.
    pub to: usize,
    /// Length of the blend in frames.
    pub frames: u64,
    /// Frames of the blend played.
    pub played: u64,
}

impl BlendView {
    /// How far the blend has gone, from 0.0 to 1.0.
    pub fn progress(&self) -> f32 {
        (self.played as f64 / self.frames.max(1) as f64) as f32
    }
}

/// Something that went wrong between commands, kept as
/// [`SetlistState::problem`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SetlistProblem {
    /// The next item could not be prepared, so it cannot follow on; the
    /// session tries again when it is selected.
    #[error("item {} could not be prepared: {error}", .item + 1)]
    Prepare {
        /// Index of the item.
        item: usize,
        /// Why.
        error: String,
    },
    /// A move changed the item after the current one, but the deck had
    /// already started handing over to the item cued before it took that
    /// item back: that item played, and the setlist follows it.
    #[error("item {} played: the move came after its hand-over had started", .item + 1)]
    MovedLate {
        /// Index of the item that played.
        item: usize,
    },
    /// An AutoLink or Crossfade found the next item not buffered in time;
    /// it started as soon as it was.
    #[error("item {} was not loaded in time to follow on; it started once it was", .item + 1)]
    LateLink {
        /// Index of the item.
        item: usize,
    },
    /// The setlist could not move on to item `item` when the current one
    /// ended, though the item itself is ready: the audio thread or the
    /// device refused the change, such as a full command queue. The current
    /// item stays selected, stopped; selecting the item again retries.
    #[error("item {} could not follow on: {error}", .item + 1)]
    Transition {
        /// Index of the item that should have followed.
        item: usize,
        /// What refused it.
        error: SetlistCommandError,
    },
}

/// Why a setlist did not open.
#[derive(Debug, Error)]
pub enum SetlistOpenError {
    /// The setlist file could not be read, or the library folder not
    /// opened.
    #[error(transparent)]
    Library(#[from] SetlistError),
    /// No item can play: each one either breaks a rule, names a song the
    /// library cannot provide, or does not build (its song does not stream,
    /// say). `problems` lists why for each item, as [`ItemInfo::problems`]
    /// would.
    #[error("setlist \"{name}\" has no item that can play")]
    NothingPlayable {
        /// The setlist's name.
        name: String,
        /// Why each item cannot play, in item order.
        problems: Vec<Vec<String>>,
    },
    /// The buffer size is refused, the device could not be opened, or
    /// building the first item met an error every item would meet (the
    /// disk thread could not start, say). An item that does not build for a
    /// reason of its own is not this error: it is reported with the items
    /// that cannot play, and the setlist opens at the next one.
    #[error(transparent)]
    Open(#[from] OpenError),
}

/// Why [`SetlistSession::send`] refused a command; nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SetlistCommandError {
    /// The current item refused the command, as a single-song session
    /// would.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    /// No playable item follows the current one.
    #[error("no item after the current one can play")]
    NoNext,
    /// No playable item comes before the current one.
    #[error("no item before the current one can play")]
    NoPrevious,
    /// There is no item at this index.
    #[error("item {} does not exist; the setlist has {items} items", .item + 1)]
    OutOfRange {
        /// The index asked for.
        item: usize,
        /// Items in the setlist.
        items: usize,
    },
    /// The item cannot play; [`ItemInfo::problems`] says why.
    #[error("item {} cannot play", .item + 1)]
    Unplayable {
        /// Index of the item.
        item: usize,
    },
    /// The item could not be prepared to play.
    #[error("item {} could not be prepared: {error}", .item + 1)]
    Prepare {
        /// Index of the item.
        item: usize,
        /// Why.
        error: String,
    },
    /// A move came while an item hands over to the next one: a crossfade
    /// runs, or the deck started the next item and the session has not
    /// followed it yet ([`SetlistSession::poll`] does). Try again once it
    /// has.
    #[error("the setlist is moving on to the next item; it can be reordered once it has")]
    HandingOver,
}

/// How a playable item is built.
#[derive(Debug, Clone)]
enum Plan {
    Song {
        folder: PathBuf,
        arrangement: String,
        positions: Option<(usize, usize)>,
        fade_out: bool,
        sample_rate: u32,
    },
    Click(ClickItem),
}

/// Why an item could not be built.
enum BuildError {
    Item(ItemError),
    Click,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Item(error) => error.fmt(f),
            Self::Click => f.write_str("the click-only tile breaks the setlist rules"),
        }
    }
}

/// An item whose player is in the deck's cue slot.
struct Staged {
    index: usize,
    tag: u64,
    info: SongInfo,
    monitor: DiskMonitor,
}

/// A cued item the session sent back, until a report shows the deck took
/// that message: the deck may have handed over to it first.
struct Withdrawn {
    staged: Staged,
    /// Messages sent once the withdrawal was.
    sent: u64,
}

/// An item prepared on the control thread, not handed over yet.
struct Pending {
    index: usize,
    item: Item,
}

/// A setlist opened from a library with its first item prepared, on no
/// device yet ([`SetlistSession::prepare`]): a shell can check the device
/// ([`Self::check`]) before it stops whatever plays there.
pub struct PreparedSetlist {
    prepared: PreparedSong,
    info: SetlistInfo,
    plans: Vec<Option<Plan>>,
    options: SessionOptions,
    first: usize,
}

impl PreparedSetlist {
    /// The setlist and what the library could provide for it.
    pub fn info(&self) -> &SetlistInfo {
        &self.info
    }

    /// Asks `output` whether it would take the setlist's device rate and
    /// buffer, without opening anything ([`AudioOutput::check`]).
    pub fn check(&self, output: &dyn AudioOutput) -> OutputCheck {
        self.prepared.check(output)
    }

    /// Opens the output device at [`SetlistInfo::sample_rate`] and starts
    /// the session at the first playable item, stopped at its start.
    pub fn start(self, output: Box<dyn AudioOutput>) -> Result<SetlistSession, OpenError> {
        let Self {
            prepared,
            info,
            plans,
            options,
            first,
        } = self;
        let session = prepared.start(output)?;
        Ok(SetlistSession {
            session,
            info,
            plans,
            options,
            current: first,
            cued: None,
            withdrawn: None,
            pending: None,
            reopen: None,
            play_after_reopen: false,
            halted: false,
            failed: None,
            blend_from: None,
            fading: None,
            finished_seen: 0,
            next_tag: 1,
            problem: None,
        })
    }
}

/// A setlist playing on one audio device: the use cases a shell calls for
/// it (S3). Shells send [`SetlistCommand`]s and draw the
/// [`SetlistSnapshot`]s [`Self::poll`] returns, about 30 times a second;
/// the session moves on only when polled.
///
/// [`SetlistSession::open`] reads a setlist from a [`Library`] and checks it
/// against the songs the library has. Before anything plays,
/// [`SetlistSession::info`] says which items cannot play (a song the library
/// lacks or cannot read, an item that breaks a setlist rule) and which
/// songs run at another sample rate than the device; neither stops the rest
/// of the setlist. The device opens at the first playable song's rate.
///
/// # How items follow each other
///
/// The session keeps the next playable item prepared while the current one
/// plays: its folder opened, its player built, its start buffered. Once it
/// is buffered it is cued on the audio thread's [`engine::live::Deck`], so
/// the current item's transition never waits for the disk:
///
/// - **Cue next**: at its end the item stops and the next one is selected,
///   stopped at its start.
/// - **Stay**: at its end the item stops and stays selected.
/// - **AutoLink**: the next item starts on the frame after the item's last.
/// - **Crossfade**: the next item starts [`domain::CROSSFADE_SECONDS`]
///   before the item's end and the two blend linearly (the gains sum to 1),
///   so two songs peaking at −1 dBFS stay below 0 dBFS.
///
/// A song at another rate than the device plays after the device reopens
/// at its rate, which stops the stream: an AutoLink or Crossfade into it
/// falls back to Cue next. An AutoLink or Crossfade whose next item was not
/// buffered in time also starts it as soon as it is, and says so
/// ([`SetlistProblem::LateLink`]). When the device refuses the other rate,
/// the session holds the item as it holds a lost device: the snapshot says
/// why ([`RuntimeError::DeviceReopenFailed`]) and the session keeps trying;
/// selecting an item at a rate the device takes brings it back.
///
/// A song whose arrangement changed after the setlist was checked, so it
/// no longer has the item's start and end positions, is refused for that
/// item ([`SetlistProblem::Prepare`]) rather than played whole. When the
/// audio thread refuses a change (its command queue is full), the setlist
/// keeps describing what it plays: the selection stays where it was, and an
/// item it had cued stays cued and followed; a transition refused that way
/// reads as [`SetlistProblem::Transition`].
///
/// A song item with a fade-out fades its stems out over its last
/// [`domain::SONG_FADE_OUT_SECONDS`], silent on its last frame; click and
/// guide play on to its end, as with the Fade control (F6.4).
///
/// # Operator commands
///
/// [`SetlistCommand::Item`] takes every command a single-song
/// [`crate::Session`] takes, for the current item (during a crossfade the
/// transport commands reach both items). Next, previous and select move to
/// another playable item, stopped at its start; while an item plays it
/// stops first, with its ramp. A click-only tile without a bar count
/// repeats its bar until the operator moves on; switching its infinite loop
/// off ends it at the bar line, and its transition follows. The jump timing
/// and fade duration the operator chose carry over to the items that
/// follow.
///
/// [`SetlistCommand::Move`] reorders the setlist while it plays: the item
/// playing goes on, and the next item is cued again when it changed. A move
/// is refused while a crossfade runs or the deck has started the next item
/// before a poll followed it ([`SetlistCommandError::HandingOver`]).
pub struct SetlistSession {
    session: Session,
    info: SetlistInfo,
    plans: Vec<Option<Plan>>,
    options: SessionOptions,
    current: usize,
    cued: Option<Staged>,
    /// The item cued last, sent back; never set while one is cued.
    withdrawn: Option<Withdrawn>,
    pending: Option<Pending>,
    /// An item at another rate waiting for the current one to stop before
    /// the device reopens.
    reopen: Option<Pending>,
    play_after_reopen: bool,
    /// Whether the operator's last transport command for the current item
    /// was a pause, stop or panic: an item that ends then is not followed
    /// by a late AutoLink or Crossfade.
    halted: bool,
    /// The next item, when preparing it failed; tried again once selected.
    failed: Option<usize>,
    /// The item a crossfade under way fades out.
    blend_from: Option<usize>,
    /// Its disk, still read while it fades out.
    fading: Option<DiskMonitor>,
    finished_seen: u64,
    next_tag: u64,
    problem: Option<SetlistProblem>,
}

impl SetlistSession {
    /// Reads the setlist file `file` from `library` (as
    /// [`Library::load_setlist`] takes it), checks it against the library's
    /// songs, prepares the first playable item and opens the output at
    /// [`SetlistInfo::sample_rate`]. It is [`Self::prepare`] followed by
    /// [`PreparedSetlist::start`].
    pub fn open(
        library: &Library,
        file: &str,
        output: Box<dyn AudioOutput>,
        options: SessionOptions,
    ) -> Result<Self, SetlistOpenError> {
        Ok(Self::prepare(library, file, options)?.start(output)?)
    }

    /// Reads and checks the setlist and prepares its first playable item,
    /// without touching any device. [`SessionOptions::arrangement`] is not
    /// used: every song item names its own.
    pub fn prepare(
        library: &Library,
        file: &str,
        options: SessionOptions,
    ) -> Result<PreparedSetlist, SetlistOpenError> {
        check_buffer(options.buffer_frames)?;
        let setlist = library.load_setlist(file)?;
        let songs = library.setlist_songs(&setlist)?;
        let errors = setlist.validate(&songs).err().unwrap_or_default();
        let (mut items, mut plans): (Vec<ItemInfo>, Vec<Option<Plan>>) = setlist
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| plan_item(library, &songs, &errors, index, item))
            .unzip();
        let built = first_buildable(&mut plans, &mut items, |plan, rate| {
            build(plan, rate, &options)
        })?;
        let Some((first, item, sample_rate)) = built else {
            return Err(SetlistOpenError::NothingPlayable {
                name: setlist.name,
                problems: items.into_iter().map(|item| item.problems).collect(),
            });
        };
        predict_fallbacks(&plans, &mut items, sample_rate);
        let info = SetlistInfo {
            name: setlist.name,
            sample_rate,
            items,
        };
        Ok(PreparedSetlist {
            prepared: PreparedSong::new(item, 0, &options),
            info,
            plans,
            options,
            first,
        })
    }

    /// The setlist and what the library could provide for it.
    pub fn info(&self) -> &SetlistInfo {
        &self.info
    }

    /// The current item's song: its arrangement as the item plays it (cut
    /// to its start and end points), or a click-only tile's one section.
    pub fn song(&self) -> &SongInfo {
        self.session.song()
    }

    /// The output devices (F8.1).
    pub fn devices(&self) -> Result<Vec<crate::DeviceInfo>, crate::OutputError> {
        self.session.devices()
    }

    /// Watches the disk threads of the current item, the item a crossfade
    /// fades out while it does, and the items prepared ahead; for tests and
    /// diagnostics.
    pub fn disk_monitors(&self) -> Vec<DiskMonitor> {
        let mut monitors = vec![self.session.disk_monitor()];
        monitors.extend(self.fading.clone());
        monitors.extend(self.cued.iter().map(|staged| staged.monitor.clone()));
        monitors.extend(
            self.withdrawn
                .iter()
                .map(|withdrawn| withdrawn.staged.monitor.clone()),
        );
        monitors.extend(
            self.pending
                .iter()
                .chain(&self.reopen)
                .map(|pending| pending.item.monitor.clone()),
        );
        monitors
    }

    /// Plays `routing` from now on, as [`crate::Session::set_routing`]
    /// does: the routing is the device's, so the current item, the cued one,
    /// an item fading out and every item after them play with it.
    pub fn set_routing(&mut self, routing: Routing) -> Result<(), RuntimeError> {
        self.session.set_routing(routing)
    }

    /// Plays with a buffer of `frames` frames from now on, as
    /// [`crate::Session::set_buffer_frames`] does, for every item.
    pub fn set_buffer_frames(&mut self, frames: u32) -> Result<(), RuntimeError> {
        self.session.set_buffer_frames(frames)?;
        self.options.buffer_frames = frames;
        Ok(())
    }

    /// The current song's folder and the `mix.json` to write there to save
    /// its mix as it plays, as [`crate::Session::mix_file`] gives them;
    /// `None` for a click-only tile.
    pub fn mix_file(&self) -> Option<(PathBuf, MixFile)> {
        self.session.mix_file()
    }

    /// The current song's mix as it plays was saved
    /// ([`crate::Session::mix_saved`]).
    pub fn mix_saved(&mut self) {
        self.session.mix_saved();
    }

    /// The global bus and master settings as they play, to save.
    pub fn bus_settings(&self) -> &BusSettings {
        self.session.bus_settings()
    }

    /// The bus settings as they play were saved
    /// ([`crate::Session::bus_settings_saved`]).
    pub fn bus_settings_saved(&mut self) {
        self.session.bus_settings_saved();
    }

    /// Sends `command`. An error from the current item is also kept as
    /// [`Snapshot::last_error`], as a single-song session keeps it.
    pub fn send(&mut self, command: SetlistCommand) -> Result<(), SetlistCommandError> {
        let target = self
            .reopen
            .as_ref()
            .map_or(self.current, |pending| pending.index);
        let item = match command {
            SetlistCommand::Item(command) => {
                // The last transport command wins, also for the item the
                // device is about to reopen for.
                match command {
                    Command::Play => {
                        self.halted = false;
                        // Play starts the item the device reopens for, not
                        // the one ramping out.
                        if self.reopen.is_some() {
                            self.play_after_reopen = true;
                            return Ok(());
                        }
                    }
                    Command::Pause | Command::Stop | Command::Panic => {
                        self.halted = true;
                        self.play_after_reopen = false;
                    }
                    _ => {}
                }
                return Ok(self.session.send(command)?);
            }
            SetlistCommand::Next => self
                .next_playable(target)
                .ok_or(SetlistCommandError::NoNext)?,
            SetlistCommand::Previous => self
                .previous_playable(target)
                .ok_or(SetlistCommandError::NoPrevious)?,
            SetlistCommand::Move { from, to } => return self.move_item(from, to),
            SetlistCommand::Select(item) => {
                if item >= self.plans.len() {
                    return Err(SetlistCommandError::OutOfRange {
                        item,
                        items: self.plans.len(),
                    });
                }
                if self.plans[item].is_none() {
                    return Err(SetlistCommandError::Unplayable { item });
                }
                item
            }
        };
        self.problem = None;
        self.failed = None;
        self.select(item, false)
    }

    /// Moves the session on and returns its state: follows the deck's
    /// handoffs, applies a transition when an item ends, reopens the device
    /// for an item at another rate once the current one has stopped,
    /// prepares and cues the next item, and does what
    /// [`crate::Session::poll`] does for the current item.
    pub fn poll(&mut self) -> SetlistSnapshot {
        let (report, now) = self.session.begin_poll();
        self.follow_deck(
            &report.deck,
            report.applied,
            report.status().transport.is_running(),
        );
        self.keep_next_ready();
        let song = self.session.end_poll(&report, now);
        let next = self.next_playable(self.current);
        let next_ready = self.next_ready(next);
        SetlistSnapshot {
            song,
            setlist: SetlistState {
                current: self.current,
                next,
                transition: self.transition(self.current),
                fallback: next.and_then(|next| {
                    rate_fallback(
                        self.transition(self.current),
                        self.session.sample_rate(),
                        next,
                        self.rate_of(next),
                    )
                }),
                next_ready,
                blend: report.deck.blend.map(|blend| BlendView {
                    from: self.blend_from.unwrap_or(self.current),
                    to: self.current,
                    frames: blend.frames,
                    played: blend.played,
                }),
                problem: self.problem.clone(),
            },
        }
    }

    /// Follows what the deck did since the last poll, with `applied`
    /// messages taken: a handoff to the cued item, an item that played to
    /// its end, a stop that a reopen waits for.
    fn follow_deck(&mut self, deck: &DeckStatus, applied: u64, running: bool) {
        if self
            .cued
            .as_ref()
            .is_some_and(|staged| staged.tag == deck.item)
            && let Some(staged) = self.cued.take()
        {
            self.follow_hand_over(staged, deck);
        }
        // The deck handed over to the item sent back before it took that
        // message: that item plays, and the setlist follows it. Once the
        // deck took the message, nothing hands over to it any more.
        if let Some(withdrawn) = self.withdrawn.take() {
            if withdrawn.staged.tag == deck.item {
                let item = withdrawn.staged.index;
                self.follow_hand_over(withdrawn.staged, deck);
                self.problem = Some(SetlistProblem::MovedLate { item });
            } else if applied < withdrawn.sent {
                self.withdrawn = Some(withdrawn);
            }
        }
        if deck.blend.is_none() {
            self.blend_from = None;
            self.fading = None;
        }
        if deck.finished > self.finished_seen {
            self.finished_seen = deck.finished;
            if deck.finished_item == Some(self.session.item()) {
                self.after_end();
            }
        }
        if !running && let Some(pending) = self.reopen.take() {
            let play = mem::take(&mut self.play_after_reopen);
            let index = pending.index;
            let tag = self.tag();
            if self.session.reopen_with(pending.item, tag).is_ok() {
                self.current = index;
                self.withdrawn = None;
                if play {
                    let _ = self.session.send(Command::Play);
                }
            }
        }
    }

    /// The deck plays `staged` now, at the end of the current item: the
    /// setlist follows it, and a crossfade under way fades the current item
    /// out.
    fn follow_hand_over(&mut self, staged: Staged, deck: &DeckStatus) {
        self.blend_from = deck.blend.map(|_| self.current);
        self.fading = deck.blend.map(|_| self.session.disk_monitor());
        self.session.bind(staged.info, staged.monitor, staged.tag);
        self.current = staged.index;
    }

    /// The current item played to its end: its transition decides what
    /// comes next. An AutoLink or Crossfade the deck did not hand over had
    /// nothing cued: the next item runs at another rate (Cue next instead),
    /// or was not buffered in time (it starts as soon as it is, unless the
    /// operator paused, stopped or panicked since the item last started).
    /// The deck counts only an end reached while playing, so an operator's
    /// stop whose ramp runs past the end never gets here.
    fn after_end(&mut self) {
        let Some(next) = self.next_playable(self.current) else {
            return;
        };
        let transition = self.transition(self.current);
        let late = match transition {
            Transition::Stay => return,
            Transition::CueNext => false,
            Transition::AutoLink | Transition::Crossfade => {
                let rate = self.session.sample_rate();
                !self.halted && rate_fallback(transition, rate, next, self.rate_of(next)).is_none()
            }
        };
        match self.select(next, late) {
            Ok(()) if late => self.problem = Some(SetlistProblem::LateLink { item: next }),
            Ok(()) => {}
            Err(error) => self.problem = Some(problem(next, error)),
        }
    }

    /// Makes `target` the current item, stopped at its start, and plays it
    /// when `play` says so. The item playing stops with its ramp.
    fn select(&mut self, target: usize, play: bool) -> Result<(), SetlistCommandError> {
        if let Some(waiting) = self.reopen.take() {
            if waiting.index == target {
                self.reopen = Some(waiting);
                self.play_after_reopen |= play;
                return Ok(());
            }
            self.play_after_reopen = false;
        }
        if target == self.current {
            self.session.send(Command::Stop)?;
            return Ok(());
        }
        // The cued record goes only once the deck took the message: a
        // refused one leaves the item cued there, and the setlist must still
        // follow its hand-over.
        if let Some(staged) = self.cued.as_ref().filter(|staged| staged.index == target) {
            self.session
                .select_cued(staged.info.clone(), staged.monitor.clone(), staged.tag)?;
            self.cued = None;
            self.current = target;
        } else {
            if self.cued.is_some() {
                self.session.uncue()?;
                self.cued = None;
            }
            let mut pending = match self.pending.take() {
                Some(pending) if pending.index == target => pending,
                _ => self.prepare_item(target)?,
            };
            let (timing, fade) = self.session.carried();
            pending.item.carry(timing, fade);
            if pending.item.info.sample_rate == self.session.sample_rate() {
                let tag = self.tag();
                self.session.select(pending.item, tag)?;
                self.current = target;
            } else if self.session.is_running() {
                // The device reopens at the item's rate once the current
                // item has ramped out.
                self.session.send(Command::Stop)?;
                self.reopen = Some(pending);
                self.play_after_reopen = play;
                return Ok(());
            } else {
                let tag = self.tag();
                self.session.reopen_with(pending.item, tag)?;
                self.current = target;
            }
        }
        // The deck plays the item selected, whatever it handed over to.
        self.withdrawn = None;
        self.blend_from = None;
        if play {
            self.session.send(Command::Play)?;
        }
        Ok(())
    }

    /// Moves item `from` to `to` ([`SetlistCommand::Move`]): every index
    /// follows its item, and the next item is cued again if it changed.
    fn move_item(&mut self, from: usize, to: usize) -> Result<(), SetlistCommandError> {
        let items = self.plans.len();
        if let Some(item) = [from, to].into_iter().find(|&item| item >= items) {
            return Err(SetlistCommandError::OutOfRange { item, items });
        }
        if from == to {
            return Ok(());
        }
        if self.handing_over() {
            return Err(SetlistCommandError::HandingOver);
        }
        let plan = self.plans.remove(from);
        self.plans.insert(to, plan);
        let item = self.info.items.remove(from);
        self.info.items.insert(to, item);
        predict_fallbacks(&self.plans, &mut self.info.items, self.info.sample_rate);
        let indexes = [
            Some(&mut self.current),
            self.cued.as_mut().map(|staged| &mut staged.index),
            self.withdrawn
                .as_mut()
                .map(|withdrawn| &mut withdrawn.staged.index),
            self.pending.as_mut().map(|pending| &mut pending.index),
            self.reopen.as_mut().map(|pending| &mut pending.index),
            self.failed.as_mut(),
            self.blend_from.as_mut(),
            self.problem.as_mut().map(|problem| match problem {
                SetlistProblem::Prepare { item, .. }
                | SetlistProblem::MovedLate { item }
                | SetlistProblem::LateLink { item }
                | SetlistProblem::Transition { item, .. } => item,
            }),
        ];
        for index in indexes.into_iter().flatten() {
            *index = moved_index(*index, from, to);
        }
        self.keep_next_ready();
        Ok(())
    }

    /// Whether an item is handing over to the next one: a crossfade runs,
    /// or the deck plays an item cued (or just sent back) that the session
    /// has not followed yet.
    fn handing_over(&mut self) -> bool {
        let deck = self.session.newest_deck();
        let unfollowed = self
            .cued
            .as_ref()
            .into_iter()
            .chain(self.withdrawn.as_ref().map(|withdrawn| &withdrawn.staged))
            .any(|staged| staged.tag == deck.item);
        self.blend_from.is_some() || deck.blend.is_some() || unfollowed
    }

    /// Prepares the next playable item ahead and cues it once its start is
    /// buffered, with the current item's transition as the handoff. A cue
    /// or preparation for another item is dropped first.
    fn keep_next_ready(&mut self) {
        if self.reopen.is_some() {
            return;
        }
        let want = self.next_playable(self.current);
        if self.failed.is_some_and(|failed| Some(failed) != want) {
            self.failed = None;
        }
        // A refused uncue keeps the record, so the setlist still follows
        // that item and cues no second one; the next poll tries again. One
        // the deck took is kept as withdrawn until a report shows the deck
        // applied it, since the deck may hand over to the item first.
        if self
            .cued
            .as_ref()
            .is_some_and(|staged| Some(staged.index) != want)
            && self.session.uncue().is_ok()
        {
            let sent = self.session.sent();
            self.withdrawn = self.cued.take().map(|staged| Withdrawn { staged, sent });
        }
        if let Some(pending) = &self.pending
            && (Some(pending.index) != want
                || pending.item.info.sample_rate != self.rate_of(pending.index))
        {
            self.pending = None;
        }
        let Some(want) = want else {
            return;
        };
        if self.cued.is_some() || self.failed == Some(want) {
            return;
        }
        if self.pending.is_none() {
            match self.prepare_item(want) {
                Ok(pending) => self.pending = Some(pending),
                Err(error) => {
                    self.failed = Some(want);
                    self.problem = Some(problem(want, error));
                    return;
                }
            }
        }
        // Nothing is cued until the deck took the last item sent back.
        if self.withdrawn.is_some() {
            return;
        }
        let rate = self.session.sample_rate();
        let ready = self.pending.as_mut().is_some_and(|pending| {
            pending.item.info.sample_rate == rate && pending.item.player.playhead_ready()
        });
        if !ready {
            return;
        }
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        let handoff = match self.transition(self.current) {
            Transition::AutoLink => Handoff::AutoLink,
            Transition::Crossfade => Handoff::Crossfade {
                frames: u64::from(CROSSFADE_SECONDS) * u64::from(rate),
            },
            Transition::CueNext | Transition::Stay => Handoff::Stop,
        };
        let (timing, fade) = self.session.carried();
        pending.item.carry(timing, fade);
        let tag = self.tag();
        let Item {
            info,
            player,
            monitor,
            mix,
            ..
        } = pending.item;
        if self.session.cue(player, mix, tag, handoff).is_ok() {
            self.cued = Some(Staged {
                index: pending.index,
                tag,
                info,
                monitor,
            });
        }
    }

    /// Builds item `index` at the device's rate.
    fn prepare_item(&self, index: usize) -> Result<Pending, SetlistCommandError> {
        let plan = self.plans.get(index).and_then(Option::as_ref);
        if plan.is_none() {
            return Err(SetlistCommandError::Unplayable { item: index });
        }
        let item = build(plan, self.session.sample_rate(), &self.options).map_err(|error| {
            SetlistCommandError::Prepare {
                item: index,
                error: error.to_string(),
            }
        })?;
        Ok(Pending { index, item })
    }

    fn next_ready(&mut self, next: Option<usize>) -> bool {
        let Some(next) = next else {
            return false;
        };
        if self
            .cued
            .as_ref()
            .is_some_and(|staged| staged.index == next)
        {
            return true;
        }
        self.pending
            .as_mut()
            .is_some_and(|pending| pending.index == next && pending.item.player.playhead_ready())
    }

    fn next_playable(&self, from: usize) -> Option<usize> {
        (from + 1..self.plans.len()).find(|&index| self.plans[index].is_some())
    }

    fn previous_playable(&self, from: usize) -> Option<usize> {
        (0..from).rev().find(|&index| self.plans[index].is_some())
    }

    fn transition(&self, index: usize) -> Transition {
        self.info.items[index].item.transition()
    }

    /// The rate item `index` plays at: a song's own, a click-only tile's
    /// the device's.
    fn rate_of(&self, index: usize) -> u32 {
        match &self.plans[index] {
            Some(Plan::Song { sample_rate, .. }) => *sample_rate,
            _ => self.session.sample_rate(),
        }
    }

    fn tag(&mut self) -> u64 {
        let tag = self.next_tag;
        self.next_tag += 1;
        tag
    }
}

impl fmt::Debug for SetlistSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetlistSession")
            .field("name", &self.info.name)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

/// The index of the item at `index` once item `from` moved to `to`.
fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < to && index > from && index <= to {
        index - 1
    } else if to < from && index >= to && index < from {
        index + 1
    } else {
        index
    }
}

/// The rate the device opens at: the first playable song's, or
/// [`CLICK_ONLY_SAMPLE_RATE`] when only click-only tiles can play.
fn device_rate(plans: &[Option<Plan>]) -> u32 {
    plans
        .iter()
        .find_map(|plan| match plan {
            Some(Plan::Song { sample_rate, .. }) => Some(*sample_rate),
            _ => None,
        })
        .unwrap_or(CLICK_ONLY_SAMPLE_RATE)
}

/// The first item that builds (`build` builds it at a device rate), the
/// item built, and the rate the device opens at. One that passed the check
/// but does not build for a reason of its own (its song does not stream,
/// say) is reported like any item that cannot play, and the next one is
/// tried. An open error every item would meet the same way (the buffer
/// size, the disk thread) ends the search with that error, as one setlist
/// problem rather than the same reason for every item. `None` when no item
/// builds.
fn first_buildable<T>(
    plans: &mut [Option<Plan>],
    items: &mut [ItemInfo],
    mut build: impl FnMut(Option<&Plan>, u32) -> Result<T, BuildError>,
) -> Result<Option<(usize, T, u32)>, OpenError> {
    loop {
        let sample_rate = device_rate(plans);
        let Some(first) = plans.iter().position(Option::is_some) else {
            return Ok(None);
        };
        match build(plans[first].as_ref(), sample_rate) {
            Ok(item) => return Ok(Some((first, item, sample_rate))),
            Err(BuildError::Item(ItemError::Open(error))) if is_session_wide(&error) => {
                return Err(error);
            }
            Err(error) => {
                items[first].problems.push(error.to_string());
                plans[first] = None;
            }
        }
    }
}

/// Whether `error`, from building one item, would stop every item the same
/// way: it comes from the session's settings or the system, not the item.
/// Output errors are not among them: building an item never opens the
/// device, and the device opens at the first item's own rate, so a refusal
/// there belongs to that item.
fn is_session_wide(error: &OpenError) -> bool {
    matches!(
        error,
        OpenError::BufferSize { .. }
            | OpenError::Load(library::LoadError::DiskThread { .. })
            | OpenError::Live(engine::live::LiveError::ZeroBufferSize)
    )
}

/// How `transition`, from an item played while the device runs at
/// `device_rate`, leads into item `next` at `next_rate`: an AutoLink or
/// Crossfade into another rate plays as Cue next, since the device reopens
/// between the two. The one rule the session plays by and reports.
fn rate_fallback(
    transition: Transition,
    device_rate: u32,
    next: usize,
    next_rate: u32,
) -> Option<RateFallback> {
    let links = matches!(transition, Transition::AutoLink | Transition::Crossfade);
    (links && next_rate != device_rate).then_some(RateFallback {
        next,
        next_rate,
        device_rate,
    })
}

/// Fills each playable item's [`ItemInfo::fallback`] for the setlist played
/// in order from a device opened at `sample_rate`: a song runs the device
/// at its own rate, a click-only tile at the rate the device runs at.
fn predict_fallbacks(plans: &[Option<Plan>], items: &mut [ItemInfo], sample_rate: u32) {
    let mut device = sample_rate;
    let rates: Vec<Option<u32>> = plans
        .iter()
        .map(|plan| match plan {
            Some(Plan::Song { sample_rate, .. }) => {
                device = *sample_rate;
                Some(device)
            }
            Some(Plan::Click(_)) => Some(device),
            None => None,
        })
        .collect();
    for (index, rate) in rates.iter().enumerate() {
        let Some(rate) = *rate else {
            continue;
        };
        let next = (index + 1..rates.len()).find_map(|next| rates[next].map(|r| (next, r)));
        items[index].fallback = next.and_then(|(next, next_rate)| {
            rate_fallback(items[index].item.transition(), rate, next, next_rate)
        });
    }
}

/// What `item` (at `index`) is, whether it can play, and how to build it.
fn plan_item(
    library: &Library,
    songs: &SetlistSongs,
    errors: &[SetlistValidationError],
    index: usize,
    item: &SetlistItem,
) -> (ItemInfo, Option<Plan>) {
    let song = match item {
        SetlistItem::Song(song) => song,
        SetlistItem::Click(click) => {
            let problems = problems_of(errors, index, false);
            let plan = problems.is_empty().then_some(Plan::Click(*click));
            let info = ItemInfo {
                item: item.clone(),
                sample_rate: None,
                problems,
                fallback: None,
            };
            return (info, plan);
        }
    };
    let unavailable = songs
        .unavailable()
        .iter()
        .find(|unavailable| unavailable.song() == song.song);
    // The library says why a song is missing; validation only that it is.
    let mut problems = problems_of(errors, index, unavailable.is_some());
    if let Some(unavailable) = unavailable {
        problems.insert(0, unavailable.to_string());
    }
    let sample_rate = songs.sample_rate(&song.song);
    let mut plan = None;
    if problems.is_empty() {
        match library.song_folder(&song.song) {
            Ok(folder) => plan = song_plan(songs, song, folder, sample_rate),
            Err(error) => problems.push(error.to_string()),
        }
    }
    if plan.is_none() && problems.is_empty() {
        problems.push(format!("song \"{}\" cannot be read", song.song));
    }
    let info = ItemInfo {
        item: item.clone(),
        sample_rate,
        problems,
        fallback: None,
    };
    (info, plan)
}

fn song_plan(
    songs: &SetlistSongs,
    song: &SongItem,
    folder: PathBuf,
    sample_rate: Option<u32>,
) -> Option<Plan> {
    let positions = songs.outline(&song.song)?.positions(&song.arrangement)?;
    let range = (song.start_position.is_some() || song.end_position.is_some()).then(|| {
        (
            song.start_position.unwrap_or(0),
            song.end_position.unwrap_or(positions.saturating_sub(1)),
        )
    });
    Some(Plan::Song {
        folder,
        arrangement: song.arrangement.clone(),
        positions: range,
        fade_out: song.fade_out,
        sample_rate: sample_rate?,
    })
}

/// The validation errors of item `index`, as text; `UnknownSong` left out
/// when the library has its own reason.
fn problems_of(errors: &[SetlistValidationError], index: usize, has_reason: bool) -> Vec<String> {
    errors
        .iter()
        .filter(|error| item_of(error) == Some(index))
        .filter(|error| {
            !(has_reason && matches!(error, SetlistValidationError::UnknownSong { .. }))
        })
        .map(ToString::to_string)
        .collect()
}

/// The item a validation error is about; `None` for the setlist as a
/// whole.
fn item_of(error: &SetlistValidationError) -> Option<usize> {
    use SetlistValidationError as E;
    match error {
        E::UnsupportedFormatVersion { .. } | E::EmptyName => None,
        E::InvalidSongPath { item, .. }
        | E::UnknownSong { item, .. }
        | E::UnknownArrangement { item, .. }
        | E::StartAfterEnd { item, .. }
        | E::PositionOutsideArrangement { item, .. }
        | E::InvalidClickBpm { item, .. }
        | E::InvalidClickNumerator { item }
        | E::InvalidClickDenominator { item, .. }
        | E::ZeroClickBars { item } => Some(*item),
    }
}

/// Builds the item `plan` describes; a click-only tile plays at `rate`.
fn build(plan: Option<&Plan>, rate: u32, options: &SessionOptions) -> Result<Item, BuildError> {
    match plan {
        Some(Plan::Song {
            folder,
            arrangement,
            positions,
            fade_out,
            ..
        }) => Item::open_item(folder, arrangement, *positions, *fade_out, options)
            .map_err(BuildError::Item),
        Some(Plan::Click(click)) => Item::click(click, rate, options).ok_or(BuildError::Click),
        None => Err(BuildError::Click),
    }
}

/// A failure to prepare item `index`, or to move on to it, as a setlist
/// problem: a preparation that failed reads as one, anything else (the
/// audio thread or the device refusing) as a refused transition.
fn problem(index: usize, error: SetlistCommandError) -> SetlistProblem {
    match error {
        SetlistCommandError::Prepare { item, error } => SetlistProblem::Prepare { item, error },
        error => SetlistProblem::Transition { item: index, error },
    }
}

#[cfg(test)]
mod tests {
    use domain::TimeSignature;
    use engine::live::LiveError;

    use super::*;

    fn click_plans(count: usize) -> (Vec<Option<Plan>>, Vec<ItemInfo>) {
        let click = ClickItem::new(
            120.0,
            TimeSignature {
                numerator: 4,
                denominator: 4,
            },
        );
        let plans = (0..count).map(|_| Some(Plan::Click(click))).collect();
        let items = (0..count)
            .map(|_| ItemInfo {
                item: SetlistItem::Click(click),
                sample_rate: None,
                problems: Vec::new(),
                fallback: None,
            })
            .collect();
        (plans, items)
    }

    fn open_error(error: OpenError) -> BuildError {
        BuildError::Item(ItemError::Open(error))
    }

    #[test]
    fn an_item_that_does_not_build_for_its_own_reason_is_skipped_and_reported() {
        let (mut plans, mut items) = click_plans(3);
        let mut tries = 0;

        let first = first_buildable(&mut plans, &mut items, |_, _| {
            tries += 1;
            match tries {
                1 => Err(open_error(OpenError::RateTooLow {
                    sample_rate: 1_000,
                    ahead_frames: 1_000,
                })),
                _ => Ok(tries),
            }
        })
        .unwrap();

        let (index, built, _) = first.expect("item 2 builds");
        assert_eq!((index, built), (1, 2));
        assert_eq!(items[0].problems.len(), 1, "{:?}", items[0].problems);
        assert!(items[0].problems[0].contains("too low to stream"));
        assert!(plans[0].is_none());
        assert!(items[1].problems.is_empty() && items[2].problems.is_empty());
    }

    #[test]
    fn an_open_error_every_item_would_meet_ends_the_search_with_it() {
        for error in [
            OpenError::BufferSize { frames: 3 },
            OpenError::Live(LiveError::ZeroBufferSize),
            OpenError::Load(library::LoadError::DiskThread {
                source: std::io::Error::other("no threads left"),
            }),
        ] {
            let (mut plans, mut items) = click_plans(3);
            let message = error.to_string();
            let mut error = Some(error);
            let mut tries = 0;

            let result = first_buildable(&mut plans, &mut items, |_, _| {
                tries += 1;
                match error.take() {
                    Some(error) => Err::<(), _>(open_error(error)),
                    None => Ok(()),
                }
            });

            assert_eq!(result.err().map(|e| e.to_string()), Some(message));
            assert_eq!(tries, 1, "no other item is tried");
            assert!(items.iter().all(|item| item.problems.is_empty()));
        }
    }

    #[test]
    fn an_output_error_from_the_first_item_is_its_own_problem_and_the_search_goes_on() {
        let (mut plans, mut items) = click_plans(2);
        let mut tries = 0;

        let first = first_buildable(&mut plans, &mut items, |_, _| {
            tries += 1;
            match tries {
                1 => Err(open_error(OpenError::Output(
                    crate::output::OutputError::NoDevice,
                ))),
                _ => Ok(()),
            }
        })
        .unwrap();

        assert_eq!(first.map(|(index, ..)| index), Some(1));
        assert_eq!(
            items[0].problems,
            ["no audio output device is available"],
            "recorded as item 1's problem"
        );
        assert!(plans[0].is_none());
    }

    /// The one-callback race no fake output can stage: the deck hands over
    /// to a cue the session already sent back after a move, before it takes
    /// that message. The cued record is moved to `withdrawn` by hand and no
    /// uncue is sent, so the deck still hands over to it.
    #[test]
    fn a_hand_over_that_beats_the_cue_sent_back_is_followed_and_explained() {
        use crate::testing::FakeOutput;
        use domain::Setlist;

        let dir = tempfile::tempdir().unwrap();
        let library = Library::new(dir.path());
        let click = |transition| {
            SetlistItem::Click(ClickItem {
                bars: Some(1),
                transition,
                ..ClickItem::new(
                    120.0,
                    TimeSignature {
                        numerator: 4,
                        denominator: 4,
                    },
                )
            })
        };
        let mut setlist = Setlist::new("Sunday");
        setlist.items = vec![
            click(Transition::AutoLink),
            click(Transition::CueNext),
            click(Transition::CueNext),
        ];
        let file = library.save_setlist(&setlist).unwrap().file;
        let fake = FakeOutput::new();
        let options = SessionOptions {
            buffer_frames: 256,
            ..SessionOptions::default()
        };
        let mut session =
            SetlistSession::open(&library, &file, Box::new(fake.clone()), options).unwrap();
        for _ in 0..100 {
            session.poll();
            if session.cued.is_some() {
                break;
            }
        }
        let staged = session.cued.take().expect("item 2 is cued");
        session.withdrawn = Some(Withdrawn {
            staged,
            sent: u64::MAX,
        });

        session.send(SetlistCommand::Item(Command::Play)).unwrap();
        // One bar at 120 BPM is two seconds.
        let bar = 2 * CLICK_ONLY_SAMPLE_RATE as usize;
        let mut state = session.poll().setlist;
        for _ in 0..(bar + 4_096) / 256 {
            fake.render(256);
            state = session.poll().setlist;
        }

        assert_eq!(state.current, 1, "the setlist follows the item that plays");
        let problem = state.problem.expect("the setlist says why");
        assert_eq!(problem, SetlistProblem::MovedLate { item: 1 });
        assert_eq!(
            problem.to_string(),
            "item 2 played: the move came after its hand-over had started"
        );
        assert!(session.withdrawn.is_none());
    }

    #[test]
    fn no_item_that_builds_is_none() {
        let (mut plans, mut items) = click_plans(2);
        let first = first_buildable(&mut plans, &mut items, |_, _| {
            Err::<(), _>(BuildError::Click)
        })
        .unwrap();
        assert!(first.is_none());
        assert!(items.iter().all(|item| item.problems.len() == 1));
    }
}
