//! Ending the show (S8): quitting or closing the window while a song plays
//! asks first, so a stray Cmd-Q or a click on the window's close button
//! cannot stop the show. Stop, the transport control, stays immediate and
//! never comes here.
//!
//! While a song is being saved (S7), ending asks first too, whatever
//! plays: quitting cancels the save, and the app ends only once the save
//! has stopped and removed what it wrote
//! ([`crate::library_page::LibraryPage::stop_save`]).
//!
//! Leaving the Song editor of a library song with edits not saved (S4)
//! asks first as well, whichever way it is left ([`Leaving`]): Discard or
//! the Library link, opening another library, quitting, or closing the
//! window.

use app::TransportState;

use crate::action::{Intent, is_playing};

/// What ends the show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// Quitting the app (Cmd-Q on the Mac, Ctrl-Q elsewhere).
    Quit,
    /// Closing the window, which also quits the app.
    CloseWindow,
}

/// What a request to end the show does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndRequest {
    /// Nothing plays: end at once; the song, if one is open, stops with
    /// its ramp first.
    EndNow,
    /// A song plays or is being saved: ask [`Ending::question`] (or
    /// [`Ending::saving_question`]) first, and end only on the answer that
    /// [`Question::ends`] the show.
    Ask,
    /// The question is already on screen: wait for its answer.
    Wait,
}

/// What a request to end the show does, given the transport of the open
/// song (`None`: no song is open), whether a song is being saved, and
/// whether the question is already on screen. A song asks while it
/// [`is_playing`]: playing, or waiting for the disk to start; a save asks
/// whatever plays.
pub fn end_request(transport: Option<TransportState>, saving: bool, asking: bool) -> EndRequest {
    if asking {
        EndRequest::Wait
    } else if saving || transport.is_some_and(is_playing) {
        EndRequest::Ask
    } else {
        EndRequest::EndNow
    }
}

/// The dialog the platform asks the question in. How it picks an answer
/// from the keyboard decides the order the answers go in, so that neither
/// Space (play/pause on stage) nor Escape ends the show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    /// The Mac's alert: Return picks the first answer, Escape the cancel
    /// answer, and Space the last one (it has the focus). "Stop and quit"
    /// goes first, "Keep playing" last as the cancel answer.
    Mac,
    /// Windows' task dialog: the first answer is the default and has the
    /// focus, so Enter and Space pick it, and Escape picks the cancel
    /// answer. "Keep playing" goes first, as the cancel answer.
    Windows,
    /// gpui's question drawn inside the window (Linux), answered with a
    /// click; the order follows the Mac. Its clicks also reach the controls
    /// beneath it, so the screen ignores them meanwhile ([`control_acts`]).
    InWindow,
}

impl Dialog {
    /// The dialog on this platform.
    pub const HERE: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(target_os = "windows") {
        Self::Windows
    } else {
        Self::InWindow
    };
}

/// One answer to the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    /// Its label.
    pub label: &'static str,
    /// Whether it keeps the show going (the song playing, the save
    /// running, the edits open); it is then the dialog's cancel answer, the
    /// one Escape picks.
    pub keeps_playing: bool,
}

/// The question asked before ending the show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Question {
    /// The question.
    pub message: &'static str,
    /// What ending does.
    pub detail: &'static str,
    /// The answers in the order the dialog lists them ([`Dialog`]).
    pub answers: [Answer; 2],
}

impl Question {
    /// Whether `answer`, the index of the answer chosen (`None` when the
    /// dialog went away unanswered), ends the show.
    pub fn ends(&self, answer: Option<usize>) -> bool {
        answer
            .and_then(|index| self.answers.get(index))
            .is_some_and(|answer| !answer.keeps_playing)
    }
}

impl Ending {
    /// The question to ask in `dialog` before ending this way while a song
    /// plays.
    pub fn question(self, dialog: Dialog) -> Question {
        let end = Answer {
            label: "Stop and quit",
            keeps_playing: false,
        };
        let keep = Answer {
            label: "Keep playing",
            keeps_playing: true,
        };
        Question {
            message: "A song is playing",
            detail: match self {
                Self::Quit => "Quitting stops the song.",
                Self::CloseWindow => "Closing the window stops the song and quits.",
            },
            answers: match dialog {
                Dialog::Mac | Dialog::InWindow => [end, keep],
                Dialog::Windows => [keep, end],
            },
        }
    }
}

impl Ending {
    /// The question to ask in `dialog` before ending this way while a song
    /// is being saved, whether or not one plays: the answers go in the same
    /// order as [`Ending::question`]'s, so neither Space nor Escape quits.
    pub fn saving_question(self, dialog: Dialog) -> Question {
        let end = Answer {
            label: "Quit",
            keeps_playing: false,
        };
        let keep = Answer {
            label: "Keep saving",
            keeps_playing: true,
        };
        Question {
            message: "A song is being saved",
            detail: match self {
                Self::Quit => {
                    "Quit anyway? The save is cancelled and nothing of the song is kept; a song that plays stops."
                }
                Self::CloseWindow => {
                    "Close the window and quit anyway? The save is cancelled and nothing of the song is kept; a song that plays stops."
                }
            },
            answers: match dialog {
                Dialog::Mac | Dialog::InWindow => [end, keep],
                Dialog::Windows => [keep, end],
            },
        }
    }
}

/// What leaves the Song editor of a library song while its edits are not
/// saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaving {
    /// Discard, or the Library link: back to the Library.
    Discard,
    /// Another library folder opening on the Library screen.
    OpenLibrary,
    /// Quitting the app, or closing its window.
    End(Ending),
}

impl Leaving {
    /// The question to ask in `dialog` before the edits are dropped: the
    /// answers go in the same order as [`Ending::question`]'s, so neither
    /// Space nor Escape drops them.
    pub fn question(self, dialog: Dialog) -> Question {
        let end = Answer {
            label: "Discard changes",
            keeps_playing: false,
        };
        let keep = Answer {
            label: "Keep editing",
            keeps_playing: true,
        };
        Question {
            message: "The song has edits that are not saved",
            detail: match self {
                Self::Discard => "Discard them and go back to the Library?",
                Self::OpenLibrary => "Opening another library discards them.",
                Self::End(Ending::Quit) => "Quitting discards them; a song that plays stops.",
                Self::End(Ending::CloseWindow) => {
                    "Closing the window discards them and quits; a song that plays stops."
                }
            },
            answers: match dialog {
                Dialog::Mac | Dialog::InWindow => [end, keep],
                Dialog::Windows => [keep, end],
            },
        }
    }
}

/// Whether `intent` acts while the question before ending the show is on
/// screen (`asking`): nothing does but the release of the panic control,
/// so a hold never stays counting. gpui's in-window question lets clicks
/// through to the controls beneath it.
pub fn control_acts(intent: Intent, asking: bool) -> bool {
    !asking || intent == Intent::PanicUp
}
