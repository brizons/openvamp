//! The controls of the Perform screen, as the `app` commands they send.

use app::{
    Command, JumpKind, JumpTiming, SetlistCommand, SetlistSnapshot, Snapshot, SongInfo,
    TransportState,
};

use crate::library_page::LibraryAction;

/// A control on the Perform screen, or the key that stands for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    /// The play/pause button: pauses while playing or loading, plays
    /// otherwise.
    TogglePlay,
    /// Stops and returns to the start.
    Stop,
    /// Moves to the start of the arrangement.
    BackToStart,
    /// A section pad: queues a jump to this arrangement position, or drops
    /// the jump when this position is the one already queued.
    QueueSection(usize),
    /// Drops the queued jump or loop.
    CancelJump,
    /// One side of the jump timing toggle.
    SetJumpTiming(JumpTiming),
    /// Switches the jump timing to the other side.
    ToggleJumpTiming,
    /// "Loop section": queues a live loop, or drops the one queued.
    ToggleLiveLoop,
    /// Infinite loop on or off.
    ToggleInfinite,
    /// Fades the track stems out, or back in.
    ToggleFade,
    /// Panic. The screen sends it only after a full hold
    /// ([`crate::hold::PanicHold`]), never on a click.
    Panic,
    /// Selects the previous playable item of the setlist (F6.1).
    PreviousSong,
    /// Selects the next playable item of the setlist (F6.1).
    NextSong,
    /// Selects the setlist item at this index, a tile of the strip.
    SelectItem(usize),
}

/// A screen of the side navigation: only the ones that exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// The Perform screen.
    Perform,
    /// The Library & setlists screen.
    Library,
    /// The Mixer screen.
    Mixer,
    /// The Audio output screen.
    Output,
}

/// What the screen's controls and keys ask the screen for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A control of the Perform screen.
    Ui(UiAction),
    /// The panic button or key went down.
    PanicDown,
    /// The panic button or key went up.
    PanicUp,
    /// "Open song…".
    OpenSong,
    /// "Open setlist…".
    OpenSetlist,
    /// A screen of the side navigation.
    ShowPage(Page),
    /// "Open library…": picks a library folder for the Library screen.
    OpenLibrary,
    /// "Import stems": picks a folder or ZIP of stems to import into the
    /// library on the Library screen.
    ImportStems,
    /// A control of the Library screen.
    Library(LibraryAction),
    /// "Perform": opens the setlist in the editor on the Perform screen.
    PerformSetlist,
    /// Hides the problems shown above a playing song.
    DismissProblems,
}

/// Whether `intent` disarms an armed remove control of the Library screen:
/// everything does but the remove controls themselves, so a pending
/// "Click again to remove …" never survives leaving the screen, a
/// transport key or any other control.
pub fn disarms_removal(intent: Intent) -> bool {
    !matches!(intent, Intent::Library(action) if action.is_removal())
}

/// The command `action` sends, given the session's state; `None` when it
/// sends nothing, such as a pad past the end of the arrangement, or a
/// setlist control with a single song open.
pub fn command_for(action: UiAction, snapshot: &Snapshot, song: &SongInfo) -> Option<Command> {
    Some(match action {
        UiAction::TogglePlay if is_playing(snapshot.transport) => Command::Pause,
        UiAction::TogglePlay => Command::Play,
        UiAction::Stop => Command::Stop,
        UiAction::BackToStart => Command::BackToStart,
        UiAction::QueueSection(position) if position >= song.positions.len() => return None,
        UiAction::QueueSection(position) => match &snapshot.queued {
            Some(queued) if queued.kind == JumpKind::Jump && queued.target == position => {
                Command::CancelJump
            }
            _ => Command::QueueJump { position },
        },
        UiAction::CancelJump => Command::CancelJump,
        UiAction::SetJumpTiming(timing) => Command::SetJumpTiming(timing),
        UiAction::ToggleJumpTiming => Command::SetJumpTiming(match snapshot.jump_timing {
            JumpTiming::SectionEnd => JumpTiming::NextBar,
            JumpTiming::NextBar => JumpTiming::SectionEnd,
        }),
        UiAction::ToggleLiveLoop if snapshot.live_loop() => Command::CancelJump,
        UiAction::ToggleLiveLoop => Command::LiveLoop,
        UiAction::ToggleInfinite => Command::SetInfinite(!snapshot.infinite),
        UiAction::ToggleFade => Command::ToggleFade,
        UiAction::Panic => Command::Panic,
        UiAction::PreviousSong | UiAction::NextSong | UiAction::SelectItem(_) => return None,
    })
}

/// The setlist command `action` sends while a setlist plays: previous, next
/// and a strip tile move through the setlist; every other control acts on
/// the current item, as [`command_for`] maps it for a single song. The
/// current item's own tile sends nothing: selecting it would stop it, which
/// a stray tap on stage must not do (back to start does that on purpose).
pub fn setlist_command_for(
    action: UiAction,
    snapshot: &SetlistSnapshot,
    song: &SongInfo,
) -> Option<SetlistCommand> {
    Some(match action {
        UiAction::PreviousSong => SetlistCommand::Previous,
        UiAction::NextSong => SetlistCommand::Next,
        UiAction::SelectItem(item) if item == snapshot.setlist.current => return None,
        UiAction::SelectItem(item) => SetlistCommand::Select(item),
        action => SetlistCommand::Item(command_for(action, &snapshot.song, song)?),
    })
}

/// Whether `transport` counts as playing: playing, or waiting for the disk
/// to start (shown as "Loading…"). Play/pause then pauses and reads
/// "Pause", and quitting or closing the window asks first
/// ([`crate::ending`]).
pub fn is_playing(transport: TransportState) -> bool {
    matches!(
        transport,
        TransportState::Playing | TransportState::Starting
    )
}
