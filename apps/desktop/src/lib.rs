//! The desktop shell (S6, S4): the Perform screen for Mac, Windows and
//! Linux, drawn with GPUI and calling the `app` crate directly, for one
//! song or a setlist, and the Library & setlists screen that edits the
//! setlists of a library folder.
//!
//! This library holds everything the screens decide without drawing, so it
//! is tested without a window: [`view`] maps a [`app::Snapshot`] to what the
//! Perform screen shows, [`facts`] reads the song facts it adds off the
//! screen's drawing, [`strip`] maps a [`app::SetlistSnapshot`] to the
//! setlist strip and its problems, [`catalog`] names a setlist's items from
//! the library's songs, [`action`] maps a control to the [`app::Command`] or
//! [`app::SetlistCommand`] it sends, [`hold`] times the hold-to-panic
//! control, [`keys`] maps the keyboard to the same controls, [`song`] opens
//! a song in place of the current one and closes it with its ramp, [`show`]
//! does the same for a song or a setlist, and [`ending`] decides when
//! quitting or closing the window asks first. For the Library screen,
//! [`library_page`] carries out its controls and says what it shows,
//! [`shelf`] lists, creates, duplicates and moves setlists, [`editor`] edits and
//! saves one, and [`line_edit`] is its one-line text field. The GPUI views
//! in the binary only draw these views and dispatch these actions.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod action;
pub mod audio_settings;
pub mod catalog;
pub mod editor;
pub mod ending;
pub mod facts;
pub mod hold;
pub mod keys;
pub mod library_page;
pub mod line_edit;
pub mod mixer;
pub mod output_page;
pub mod reorder;
pub mod shelf;
pub mod show;
pub mod song;
pub mod strip;
pub mod view;
