//! S8: quitting or closing the window while a song plays asks first. Stop,
//! the transport control, stays immediate: it never goes through these
//! rules (its command is in view_model.rs).

use app::TransportState;
use openvamp_desktop::action::{Intent, UiAction, is_playing};
use openvamp_desktop::ending::{Dialog, EndRequest, Ending, control_acts, end_request};

const ALL: [TransportState; 7] = [
    TransportState::Stopped,
    TransportState::Starting,
    TransportState::Playing,
    TransportState::Pausing,
    TransportState::Paused,
    TransportState::Stopping,
    TransportState::Panicking,
];

const DIALOGS: [Dialog; 3] = [Dialog::Mac, Dialog::Windows, Dialog::InWindow];

#[test]
fn playing_or_waiting_for_the_disk_counts_as_playing() {
    for transport in ALL {
        let expected = matches!(
            transport,
            TransportState::Playing | TransportState::Starting
        );
        assert_eq!(is_playing(transport), expected, "{transport:?}");
    }
}

#[test]
fn ending_asks_first_only_while_a_song_plays() {
    for transport in ALL {
        let expected = if is_playing(transport) {
            EndRequest::Ask
        } else {
            EndRequest::EndNow
        };
        assert_eq!(
            end_request(Some(transport), false, false),
            expected,
            "{transport:?}"
        );
    }
    assert_eq!(
        end_request(None, false, false),
        EndRequest::EndNow,
        "no song"
    );
}

#[test]
fn a_second_request_waits_for_the_question_on_screen() {
    for (transport, saving) in ALL
        .map(Some)
        .into_iter()
        .chain([None])
        .flat_map(|transport| [(transport, false), (transport, true)])
    {
        assert_eq!(
            end_request(transport, saving, true),
            EndRequest::Wait,
            "{transport:?}"
        );
    }
}

/// The labels in the order the dialog lists them, and which ends the show.
fn order(dialog: Dialog) -> Vec<(&'static str, bool)> {
    let question = Ending::Quit.question(dialog);
    question
        .answers
        .iter()
        .enumerate()
        .map(|(index, answer)| {
            assert_eq!(answer.keeps_playing, !question.ends(Some(index)));
            (answer.label, question.ends(Some(index)))
        })
        .collect()
}

#[test]
fn on_the_mac_return_ends_and_escape_or_space_keep_playing() {
    // NSAlert: Return picks the first answer, Escape the cancel answer,
    // and the last one has the focus (Space).
    assert_eq!(
        order(Dialog::Mac),
        [("Stop and quit", true), ("Keep playing", false)]
    );
}

#[test]
fn on_windows_keep_playing_is_the_default_answer() {
    // The task dialog's first answer is the default with the focus: Enter
    // and Space pick it. Escape picks the cancel answer, the same one.
    assert_eq!(
        order(Dialog::Windows),
        [("Keep playing", false), ("Stop and quit", true)]
    );
}

#[test]
fn the_in_window_question_lists_them_as_the_mac_does() {
    assert_eq!(order(Dialog::InWindow), order(Dialog::Mac));
}

#[test]
fn the_dialog_here_is_this_platforms() {
    let expected = if cfg!(target_os = "macos") {
        Dialog::Mac
    } else if cfg!(target_os = "windows") {
        Dialog::Windows
    } else {
        Dialog::InWindow
    };
    assert_eq!(Dialog::HERE, expected);
}

#[test]
fn only_the_ending_answer_ends_the_show() {
    for dialog in DIALOGS {
        let question = Ending::Quit.question(dialog);
        let ending = question.answers.iter().filter(|a| !a.keeps_playing).count();
        assert_eq!(ending, 1, "{dialog:?}");
        assert!(
            !question.ends(None),
            "a dialog that went away keeps playing"
        );
        assert!(!question.ends(Some(7)), "{dialog:?}");
    }
}

#[test]
fn the_question_says_what_ending_does() {
    for dialog in DIALOGS {
        for ending in [Ending::Quit, Ending::CloseWindow] {
            assert_eq!(ending.question(dialog).message, "A song is playing");
        }
        assert_eq!(
            Ending::Quit.question(dialog).detail,
            "Quitting stops the song."
        );
        assert_eq!(
            Ending::CloseWindow.question(dialog).detail,
            "Closing the window stops the song and quits."
        );
    }
}

#[test]
fn while_the_question_is_open_the_controls_beneath_it_do_nothing() {
    let intents = [
        Intent::Ui(UiAction::TogglePlay),
        Intent::Ui(UiAction::Stop),
        Intent::Ui(UiAction::QueueSection(0)),
        Intent::Ui(UiAction::Panic),
        Intent::PanicDown,
        Intent::OpenSong,
        Intent::DismissProblems,
    ];
    for intent in intents {
        assert!(control_acts(intent, false), "{intent:?}");
        // gpui's in-window question lets clicks through to the screen.
        assert!(!control_acts(intent, true), "{intent:?}");
    }
    // A release always reaches the hold, so it never stays counting.
    assert!(control_acts(Intent::PanicUp, true));
    assert!(control_acts(Intent::PanicUp, false));
}

#[test]
fn ending_asks_first_while_a_song_is_being_saved_whatever_plays() {
    for transport in ALL.map(Some).into_iter().chain([None]) {
        assert_eq!(
            end_request(transport, true, false),
            EndRequest::Ask,
            "{transport:?}"
        );
    }
}

#[test]
fn the_question_while_saving_offers_keep_saving_as_its_cancel_answer() {
    for ending in [Ending::Quit, Ending::CloseWindow] {
        for dialog in DIALOGS {
            let question = ending.saving_question(dialog);
            assert_eq!(question.message, "A song is being saved", "{ending:?}");
            let labels: Vec<(&str, bool)> = question
                .answers
                .iter()
                .enumerate()
                .map(|(index, answer)| {
                    assert_eq!(answer.keeps_playing, !question.ends(Some(index)));
                    (answer.label, question.ends(Some(index)))
                })
                .collect();
            let expected = match dialog {
                Dialog::Mac | Dialog::InWindow => [("Quit", true), ("Keep saving", false)],
                Dialog::Windows => [("Keep saving", false), ("Quit", true)],
            };
            assert_eq!(labels, expected, "{ending:?} {dialog:?}");
            assert!(
                !question.ends(None),
                "a dialog gone unanswered keeps saving"
            );
        }
    }
    assert_eq!(
        Ending::Quit.saving_question(Dialog::Mac).detail,
        "Quit anyway? The save is cancelled and nothing of the song is kept; a song that plays stops."
    );
    assert_eq!(
        Ending::CloseWindow.saving_question(Dialog::Mac).detail,
        "Close the window and quit anyway? The save is cancelled and nothing of the song is kept; a song that plays stops."
    );
}
