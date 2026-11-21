//! One turn of the screens' timer without a window ([`Timed::turn`]): it
//! moves on everything the screens show that changes with time alone, and
//! says whether they must draw again. A turn that changes nothing on show
//! draws nothing, so a still screen with every meter at 0 costs no frames.
//!
//! What changes with time alone, screen by screen, and how a turn sees it:
//! - every screen: the session the turn polls (a song that plays, meters
//!   that fall, a device lost or back, a setlist that moves on by itself),
//!   seen in the song's snapshot and the setlist's state ([`Drawn`]), and
//!   drawn on every turn while the song moves or a meter reads above
//!   silence ([`needs_frames`]); a hold of the Panic control, drawn on
//!   every turn while it fills ([`PanicHold::is_counting`]);
//! - Mixer: bus and master changes saved [`crate::mixer::BUS_SAVE_DELAY`]
//!   after the last one, or the reason they could not be
//!   ([`MixerPage::buses_waiting`], in [`Drawn`]);
//! - Audio output: a buffer size or a routing waiting for the device,
//!   settled by the session or, at the latest,
//!   [`crate::output_page::APPLY_PATIENCE`] after it was asked for
//!   ([`OutputPage::settling`], in [`Drawn`]); it counts whichever screen
//!   shows, at the cost of a frame drawn for nothing on the others;
//! - Perform: the song facts, read again [`crate::facts::RETRY_AFTER`]
//!   after a failed read. The screen starts the read on a turn, but what
//!   shows changes only when it comes back, and the screen draws then;
//! - Library: nothing; it changes only with its controls.
//!
//! The notices and problems of every screen stay until a control or a
//! change clears them; none times out. Tooltips are gpui's own: it shows
//! and hides them on its own timer and draws them itself.

use std::time::Instant;

use app::{SetlistCommandError, SetlistState, Snapshot};

use crate::action::UiAction;
use crate::audio_settings::AudioSettings;
use crate::hold::PanicHold;
use crate::mixer::{MixerPage, needs_frames};
use crate::output_page::{OutputPage, Settling};
use crate::show::Show;

/// What the screens draw that a turn of their timer can change with no
/// control touched, kept across the turn to tell whether it changed
/// anything on show: the song's state and, for a setlist, where it stands
/// and the control it last refused; whether bus changes wait to be saved;
/// and what the Audio output screen says of the changes waiting for the
/// device.
#[derive(Debug, PartialEq)]
pub struct Drawn {
    song: Snapshot,
    setlist: Option<(SetlistState, Option<SetlistCommandError>)>,
    buses_waiting: bool,
    output: Settling,
}

impl Drawn {
    /// What the screens draw now from `show`, `mixer` and `output`.
    pub fn of(show: &Show, mixer: &MixerPage, output: &OutputPage) -> Self {
        Self {
            song: show.snapshot().clone(),
            setlist: show
                .setlist()
                .map(|setlist| (setlist.snapshot.setlist.clone(), setlist.refused.clone())),
            buses_waiting: mixer.buses_waiting(),
            output: output.settling(),
        }
    }
}

/// Everything a turn of the screens' timer moves on.
pub struct Timed<'a> {
    /// What plays, if anything.
    pub show: Option<&'a mut Show>,
    /// The hold of the Panic control.
    pub hold: &'a mut PanicHold,
    /// The Mixer screen's state, with its bus changes waiting to be saved.
    pub mixer: &'a mut MixerPage,
    /// The Audio output screen's state, with its changes waiting for the
    /// device.
    pub output: &'a mut OutputPage,
    /// The settings bus and output changes are saved to.
    pub settings: &'a mut AudioSettings,
}

impl Timed<'_> {
    /// One turn at `now`: fires a hold of Panic that is complete, saves bus
    /// changes that have settled, moves the session on and settles the
    /// output changes waiting for the device. Returns whether the screens
    /// draw again: while a hold fills or the song needs frames
    /// ([`needs_frames`]), else only when the turn changed what they draw
    /// ([`Drawn`]).
    pub fn turn(self, now: Instant) -> bool {
        let counting = self.hold.is_counting();
        let fires = self.hold.tick(now);
        let Some(show) = self.show else {
            // Nothing moves without a song, except a hold's fill.
            return counting;
        };
        // What the screens drew from, unless they draw this turn anyway.
        let before =
            (!needs_frames(show.snapshot())).then(|| Drawn::of(show, self.mixer, self.output));
        self.mixer.tick(show, self.settings, now);
        if fires {
            show.send(UiAction::Panic);
        }
        show.poll();
        self.output.follow(Some(&mut *show), self.settings, now);
        counting
            || needs_frames(show.snapshot())
            || before.is_none_or(|before| before != Drawn::of(show, self.mixer, self.output))
    }
}
