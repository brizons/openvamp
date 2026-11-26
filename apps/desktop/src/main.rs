//! `openvamp`: the desktop shell for Mac, Windows and Linux (S6).
//!
//! Opens the Perform screen, with the song folder or setlist file given on
//! the command line when there is one, on the system's default audio
//! output.

#![forbid(unsafe_code)]

mod library_ui;
mod mixer_ui;
mod output_ui;
mod screen;
mod theme;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use gpui::Application;

const USAGE: &str = "\
usage: openvamp [SONG_DIR]
       openvamp SETLIST_FILE

Opens the Perform screen. SONG_DIR is a song folder: song.json and the WAV
stems it lists; without it, use \"Open song…\" (Cmd/Ctrl-O) or \"Open
setlist…\". SETLIST_FILE is a file ending in .setlist.json in the Setlists
folder of a library folder, whose songs are in that library folder. Songs
play on the system's default audio output, at their sample rate.";

/// The exit code when the window cannot open. macOS ends the app from
/// inside its run loop, so this exits the process at once; nothing has
/// opened a song or a device by then.
const WINDOW_FAILED: i32 = 1;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().map(PathBuf::from);
    let extra: Vec<String> = args
        .map(|arg| format!("{:?}", arg.to_string_lossy()))
        .collect();
    if !extra.is_empty() {
        let noun = if extra.len() == 1 {
            "argument"
        } else {
            "arguments"
        };
        eprintln!(
            "unexpected {noun} {} (one song folder or setlist file at most)\n\n{USAGE}",
            extra.join(" ")
        );
        return ExitCode::from(2);
    }
    if let Some(path) = &path
        && let Some(flag) = path.to_str().filter(|arg| arg.starts_with('-'))
    {
        if flag == "-h" || flag == "--help" {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        eprintln!("unknown option {flag}\n\n{USAGE}");
        return ExitCode::from(2);
    }

    Application::new().with_assets(theme::Icons).run(move |cx| {
        theme::load_fonts(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = screen::open_window(path, cx) {
            eprintln!("error: cannot open the window: {error}");
            std::process::exit(WINDOW_FAILED);
        }
        cx.activate(true);
    });
    ExitCode::SUCCESS
}
