//! The Perform screen's presentational code: draws a [`PerformView`], and
//! for a setlist a [`SetlistView`], after the design canvas's Main artboard
//! (reduced to S6 and S4), with the compact bus mixer of the Mixer screen
//! under it, and reports what the person does as an [`Intent`]. It decides
//! nothing. The canvas has no status bar: the device and its underruns are
//! on the header's device chip, a problem with the song or the device shows
//! in the notes under the strip, and each control names its key in the
//! hint it shows when the pointer rests on it.

use app::JumpTiming;
use domain::Transition;
use gpui::{
    AnyElement, AnyView, App, Bounds, ClickEvent, Context, Div, DragMoveEvent, Element, ElementId,
    Entity, FocusHandle, FontWeight, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    MouseButton, Pixels, Point, Rgba, ShapedLine, SharedString, Size, Stateful, Style, TextRun,
    Window, div, prelude::*, px, relative, svg,
};
use openvamp_desktop::action::UiAction;
use openvamp_desktop::mixer::MixerScreen;
use openvamp_desktop::reorder::{Half, half_at};
use openvamp_desktop::strip::{ItemState, SetlistView, StripItem, TransitionChip};
use openvamp_desktop::view::{
    DeviceChip, FadeButton, Level, NextCard, NowCard, PAD_GAP, Pad, PadGrid, PerformView,
    SectionKind, SongHeader, SongMap, TransportView, pad_grid,
};

use openvamp_desktop::action::{Intent, Page};

use crate::mixer_ui;
use crate::screen::Perform;
use crate::theme;

/// How the empty screen names the open shortcut.
const OPEN_HINT: &str = if cfg!(target_os = "macos") {
    "⌘O opens a song at any time"
} else {
    "Ctrl-O opens a song at any time"
};

/// The open-song key, as the hints name it.
const OPEN_KEY: &str = if cfg!(target_os = "macos") {
    "⌘O"
} else {
    "Ctrl-O"
};

/// The side navigation's width, in points.
const NAV_WIDTH: f32 = 64.0;

/// The room around a screen, in points ([`main`]).
const MAIN_PADDING: f32 = 16.0;

/// The room between the parts of a screen, in points.
const MAIN_GAP: f32 = 12.0;

/// The border of a panel drawn with `border_1`, in points, on each side.
const PANEL_BORDER: f32 = 1.0;

/// The header's height, its line under it included.
const HEADER_HEIGHT: f32 = 60.0;

/// The setlist strip's height.
const STRIP_HEIGHT: f32 = 84.0;

/// The now-playing panel's padding, in points, inside its border.
const PANEL_PADDING: f32 = 14.0;

/// The room between the rows of the now-playing panel, in points.
const PANEL_GAP: f32 = 12.0;

/// The now-playing panel's first row: the title and its facts, Now and
/// Next.
const TITLE_ROW_HEIGHT: f32 = 60.0;

/// The song map's row of section blocks.
const MAP_BLOCKS_HEIGHT: f32 = 28.0;

/// The song map's track under the blocks, with the playhead.
const MAP_TRACK_HEIGHT: f32 = 50.0;

/// The song map's bar ruler under the track.
const MAP_RULER_HEIGHT: f32 = 14.0;

/// The room between the song map's blocks, track and ruler, in points.
const MAP_GAP: f32 = 6.0;

/// The song map: its blocks, track and ruler.
const SONG_MAP_HEIGHT: f32 =
    MAP_BLOCKS_HEIGHT + MAP_TRACK_HEIGHT + MAP_RULER_HEIGHT + 2.0 * MAP_GAP;

/// The section controls' row: Jump at, Loop section and Infinite.
const SECTION_CONTROLS_HEIGHT: f32 = 34.0;

/// The pads' area in the now-playing panel above the compact mixer: one row
/// of pads, the rest scrolls.
const PADS_HEIGHT: f32 = 100.0;

/// The now-playing panel's height above the compact mixer, as on the
/// design canvas (364 points): the title row, the song map, the section
/// controls and one row of pads, with the room between them and the
/// panel's padding and border.
const NOW_PLAYING_HEIGHT: f32 = TITLE_ROW_HEIGHT
    + SONG_MAP_HEIGHT
    + SECTION_CONTROLS_HEIGHT
    + PADS_HEIGHT
    + 3.0 * PANEL_GAP
    + 2.0 * (PANEL_PADDING + PANEL_BORDER);

/// The least height the compact mixer needs for its bus strips; with less
/// room it stays off Perform (the Mixer screen has it all).
const COMPACT_MIXER_MIN_HEIGHT: f32 = 272.0;

/// The size of a square button, such as the one that hides the problems.
const SQUARE_BUTTON_SIZE: f32 = 48.0;

/// The problems' padding above and below their lines, in points.
const PROBLEMS_PADDING_Y: f32 = 10.0;

/// The notes' padding above and below their lines, in points.
const NOTES_PADDING_Y: f32 = 8.0;

/// The type size of a line of problems or notes.
const NOTE_TEXT_SIZE: f32 = 13.0;

/// A line of problems or notes, in points, as drawn: about the default
/// line of [`NOTE_TEXT_SIZE`] type (1.618 em).
const NOTE_LINE_HEIGHT: f32 = 21.0;

/// The room between lines of problems or notes, in points.
const NOTE_GAP: f32 = 4.0;

/// The notes panel's tallest, its padding and border included, in points;
/// more lines scroll.
const NOTES_MAX_HEIGHT: f32 = 110.0;

/// The window's root: background, type, and the keyboard focus; the side
/// navigation and [`main`] go in it.
pub fn root(focus: &FocusHandle, cx: &mut Context<Perform>) -> Div {
    let focus_on_click = focus.clone();
    div()
        .track_focus(focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, _, window, _| focus_on_click.focus(window)),
        )
        .size_full()
        .flex()
        .bg(theme::bg())
        .font_family(theme::UI)
        .text_color(theme::text())
        .text_size(px(15.))
}

/// The area beside the side navigation that holds a screen.
pub fn main() -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(MAIN_GAP))
        .p(px(MAIN_PADDING))
}

/// The side navigation of the design canvas, with the screens that exist:
/// Perform, Library and Mixer, and Audio output at the bottom.
pub fn nav(page: Page, cx: &mut Context<Perform>) -> impl IntoElement {
    let item = |id: &'static str, name: &'static str, target: Page, cx: &mut Context<Perform>| {
        let (bg, fg) = if page == target {
            (theme::badge(), theme::text())
        } else {
            (theme::panel(), theme::text_muted())
        };
        button(id, cx, Intent::ShowPage(target))
            .size(px(48.))
            .justify_center()
            .rounded(px(12.))
            .bg(bg)
            .child(icon(name, fg, 22.))
    };
    div()
        .w(px(NAV_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .py(px(16.))
        .bg(theme::panel())
        .border_r_1()
        .border_color(theme::border())
        .child(item("nav-perform", "perform", Page::Perform, cx))
        .child(item("nav-library", "library", Page::Library, cx))
        .child(item("nav-mixer", "mixer", Page::Mixer, cx))
        .child(div().flex_1())
        .child(item("nav-output", "output", Page::Output, cx))
}

/// Covers the whole screen while the question before ending the show is
/// open: gpui's in-window question (Linux) lets clicks through, and this
/// keeps them from the controls beneath it.
pub fn shield() -> impl IntoElement {
    div().absolute().top_0().left_0().size_full().occlude()
}

/// The screen with a song or a setlist open, and the `problems` that did
/// not stop it, in a window whose content is `viewport`; `mixer` is the
/// Mixer screen's view of the song, whose buses show under the pads when
/// the window leaves them room.
pub fn perform(
    view: &PerformView,
    setlist: Option<&SetlistView>,
    mixer: Option<&MixerScreen>,
    viewport: Size<Pixels>,
    hold: f32,
    problems: &[String],
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let notes = notes(view, setlist);
    let mixer = mixer.filter(|_| {
        mixer_room(
            f32::from(viewport.height),
            setlist.is_some(),
            problems,
            &notes,
        ) >= COMPACT_MIXER_MIN_HEIGHT
    });
    // The pads' row: the window less the navigation, the screen's padding,
    // and the panel's padding and border.
    let row = f32::from(viewport.width)
        - NAV_WIDTH
        - 2.0 * MAIN_PADDING
        - 2.0 * (PANEL_PADDING + PANEL_BORDER);
    let grid = pad_grid(row, view.pads.len());
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(MAIN_GAP))
        .child(header(view, setlist, hold, cx))
        .when(!problems.is_empty(), |screen| {
            screen.child(problem_banner(problems, cx))
        })
        .when_some(setlist, |screen, setlist| screen.child(strip(setlist, cx)))
        .when(!notes.is_empty(), |screen| {
            screen.child(notes_panel(&notes))
        })
        .child(now_playing(view, grid, mixer.is_some(), cx))
        .when_some(mixer, |screen, mixer| {
            screen.child(mixer_ui::compact_mixer(mixer, cx))
        })
}

/// The room the compact mixer would get in a window `height` points tall,
/// under the header, the problems, the strip with a setlist, the notes and
/// the now-playing panel, each with the room after it. A line of problems
/// or notes counts as one line, even when it wraps.
fn mixer_room(height: f32, setlist: bool, problems: &[String], notes: &[Note]) -> f32 {
    let lines =
        |count: usize| count as f32 * NOTE_LINE_HEIGHT + count.saturating_sub(1) as f32 * NOTE_GAP;
    // The problems are at least as tall as the button that hides them.
    let problems = if problems.is_empty() {
        0.0
    } else {
        lines(problems.len()).max(SQUARE_BUTTON_SIZE)
            + 2.0 * (PROBLEMS_PADDING_Y + PANEL_BORDER)
            + MAIN_GAP
    };
    let strip = if setlist {
        STRIP_HEIGHT + MAIN_GAP
    } else {
        0.0
    };
    let notes = if notes.is_empty() {
        0.0
    } else {
        (lines(notes.len()) + 2.0 * (NOTES_PADDING_Y + PANEL_BORDER)).min(NOTES_MAX_HEIGHT)
            + MAIN_GAP
    };
    height
        - 2.0 * MAIN_PADDING
        - HEADER_HEIGHT
        - MAIN_GAP
        - problems
        - strip
        - notes
        - NOW_PLAYING_HEIGHT
        - MAIN_GAP
}

/// Problems that did not stop the song playing, such as a pick that did
/// not open, with a button that hides them.
fn problem_banner(problems: &[String], cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .py(px(PROBLEMS_PADDING_Y))
        .bg(theme::danger_bg())
        .border_1()
        .border_color(theme::danger())
        .rounded(px(14.))
        .text_size(px(NOTE_TEXT_SIZE))
        .line_height(px(NOTE_LINE_HEIGHT))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(NOTE_GAP))
                .text_color(theme::danger_text())
                .children(problems.iter().map(|line| div().child(line.clone()))),
        )
        .child(
            square_button("dismiss-problems", cx, Intent::DismissProblems).child(icon(
                "close",
                theme::danger_text(),
                18.,
            )),
        )
}

/// The screen with no song: why, and the way to open one.
pub fn no_song(errors: &[String], cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(14.))
        .child(
            div()
                .text_size(px(26.))
                .font_weight(FontWeight::BOLD)
                .child("No song loaded"),
        )
        .child(
            div()
                .text_color(theme::text_muted())
                .child("Open a song folder: song.json and the WAV stems it lists."),
        )
        .children(errors.iter().map(|line| {
            div()
                .max_w(px(900.))
                .text_color(theme::danger())
                .child(line.clone())
        }))
        .child(
            button("open-empty", cx, Intent::OpenSong)
                .h(px(48.))
                .px(px(18.))
                .bg(theme::text())
                .text_color(theme::on_fill())
                .font_weight(FontWeight::BOLD)
                .child(icon("folder", theme::on_fill(), 20.))
                .child("Open song…"),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::text_faint())
                .child(OPEN_HINT),
        )
}

/// The header of the design canvas: the setlist (or the open buttons for a
/// song alone), the transport, the clock, the device chip, fade and panic;
/// the app's mark goes first, which the canvas does not have.
fn header(
    view: &PerformView,
    setlist: Option<&SetlistView>,
    hold: f32,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let header = div()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .pb(px(12.))
        .border_b_1()
        .border_color(theme::divider())
        .child(brand_mark());
    let header = match setlist {
        Some(setlist) => header.child(setlist_chip(setlist, cx)),
        None => header
            .child(
                chip_button("open", cx, Intent::OpenSong)
                    .h(px(46.))
                    .tooltip(hint(format!("Open a song folder · {OPEN_KEY}")))
                    .child(icon("folder", theme::text_muted(), 20.))
                    .child("Open song…"),
            )
            .child(
                chip_button("open-setlist", cx, Intent::OpenSetlist)
                    .h(px(46.))
                    .child(icon("setlist", theme::text_muted(), 20.))
                    .child("Open setlist…"),
            ),
    };
    header
        .child(transport(&view.transport, setlist, cx))
        .child(clock(view))
        .child(div().flex_1())
        .child(device_chip(&view.device, cx))
        .child(fade(&view.fade, cx))
        .child(panic(hold, cx))
}

/// The setlist's name and the current item's place; picks another
/// setlist.
fn setlist_chip(setlist: &SetlistView, cx: &mut Context<Perform>) -> impl IntoElement {
    // It gives way to the transport and the clock, but never below a width
    // that keeps the start of the name readable.
    chip_button("setlist", cx, Intent::OpenSetlist)
        .h(px(46.))
        .px(px(10.))
        .gap(px(10.))
        .flex_shrink()
        .min_w(px(140.))
        .max_w(px(260.))
        .tooltip(hint(format!("Change setlist · {OPEN_KEY} opens a song")))
        .child(icon("setlist", theme::text_muted(), 20.))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(16.))
                        .child(one_line_text(setlist.name.clone())),
                )
                .child(
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(11.))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::text_muted())
                        .whitespace_nowrap()
                        .child(setlist.position.clone()),
                ),
        )
        .child(icon("down", theme::text(), 16.))
}

/// Back to start, play/pause and stop; with a setlist, previous and next
/// song around play, as on the design canvas. Play/pause is an icon.
fn transport(
    transport: &TransportView,
    setlist: Option<&SetlistView>,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let step = |name: &'static str,
                action: UiAction,
                enabled: bool,
                tip: &'static str,
                cx: &mut Context<Perform>| {
        let color = if enabled {
            theme::text()
        } else {
            theme::text_faint()
        };
        header_button(name, cx, Intent::Ui(action))
            .tooltip(hint(tip))
            .child(icon(name, color, 20.))
    };
    let previous = setlist.map(|setlist| {
        step(
            "previous",
            UiAction::PreviousSong,
            setlist.has_previous,
            "Previous song · ,",
            cx,
        )
    });
    let next = setlist.map(|setlist| {
        step(
            "next",
            UiAction::NextSong,
            setlist.has_next,
            "Next song · .",
            cx,
        )
    });
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            header_button("back", cx, Intent::Ui(UiAction::BackToStart))
                .tooltip(hint("Back to start · B"))
                .child(icon("back-to-start", theme::text(), 20.)),
        )
        .children(previous)
        .child(
            button("play", cx, Intent::Ui(UiAction::TogglePlay))
                .w(px(64.))
                .h(px(48.))
                .justify_center()
                .rounded(px(14.))
                .bg(theme::accent())
                .tooltip(hint(format!("{} · Space", transport.play_label)))
                .child(icon(
                    if transport.playing { "pause" } else { "play" },
                    theme::on_fill(),
                    26.,
                )),
        )
        .children(next)
        .child(
            header_button("stop", cx, Intent::Ui(UiAction::Stop))
                .tooltip(hint("Stop · S"))
                .child(icon("stop", theme::text(), 20.)),
        )
}

/// A 44 point square button of the header.
fn header_button(id: &'static str, cx: &mut Context<Perform>, intent: Intent) -> Stateful<Div> {
    square_button(id, cx, intent).size(px(44.))
}

/// The time and the length of the arrangement, and the bar and beat, with
/// what the transport is doing while it changes state.
fn clock(view: &PerformView) -> impl IntoElement {
    let bar = match &view.transport.note {
        Some(note) => format!("{} · {note}", view.bar_beat),
        None => view.bar_beat.clone(),
    };
    div()
        .flex_none()
        .min_w(px(128.))
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_baseline()
                .whitespace_nowrap()
                .font_family(theme::MONO)
                .child(
                    div()
                        .text_size(px(22.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(view.time.clone()),
                )
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(theme::text_faint())
                        .child(format!("\u{a0}/ {}", view.total)),
                ),
        )
        .child(
            div()
                .font_family(theme::MONO)
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .whitespace_nowrap()
                .child(bar),
        )
}

/// The device, its outputs, buffer and latency, with a dot that says
/// whether it plays; opens the Audio output screen.
fn device_chip(device: &DeviceChip, cx: &mut Context<Perform>) -> impl IntoElement {
    let dot = match device.level {
        Level::Ok => theme::accent(),
        Level::Warning => theme::queued(),
        Level::Error => theme::danger(),
    };
    button("device", cx, Intent::ShowPage(Page::Output))
        .flex_shrink()
        .min_w_0()
        .max_w(px(300.))
        .h(px(44.))
        .pl(px(10.))
        .pr(px(12.))
        .gap(px(10.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .tooltip(hint("Audio output settings"))
        .child(
            div()
                .relative()
                .flex_none()
                .flex()
                .child(icon("device", theme::text_soft(), 22.))
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .right(px(-5.))
                        .size(px(12.))
                        .rounded(px(6.))
                        .border_2()
                        .border_color(theme::surface())
                        .bg(dot),
                ),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(one_line_text(device.name.clone())),
                )
                .child(
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(one_line_text(device.meta.clone())),
                ),
        )
}

fn fade(fade: &FadeButton, cx: &mut Context<Perform>) -> impl IntoElement {
    let fg = if fade.active {
        theme::on_fill()
    } else {
        theme::text()
    };
    let button = button("fade", cx, Intent::Ui(UiAction::ToggleFade))
        .h(px(44.))
        .px(px(14.))
        .text_color(fg)
        .text_size(px(15.))
        .font_weight(FontWeight::SEMIBOLD)
        .tooltip(hint("Fade out or in · F"));
    let button = if fade.active {
        button.bg(theme::text())
    } else {
        button
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
    };
    button
        .child(icon("fade", fg, 20.))
        .child(fade.label.clone())
}

/// Panic fires only after a full hold; the fill shows how far it is.
fn panic(hold: f32, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .id("panic")
        .flex_none()
        .relative()
        .overflow_hidden()
        .h(px(44.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(10.))
        .border_1()
        .border_color(theme::danger())
        .bg(theme::danger_bg())
        .text_color(theme::danger_text())
        .text_size(px(15.))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .tooltip(hint("Hold to fade everything out and stop · hold X"))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|screen, _, window, cx| screen.handle(Intent::PanicDown, window, cx)),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|screen, _, window, cx| screen.handle(Intent::PanicUp, window, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|screen, _, window, cx| screen.handle(Intent::PanicUp, window, cx)),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(hold))
                .bg(theme::danger_fill()),
        )
        .child(icon("panic", theme::danger_text(), 20.))
        .child("Panic")
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::danger_hint())
                .child("hold"),
        )
}

/// The song's title and facts, Now and Next, the song map, the section
/// controls and the pads. With the compact mixer under it (`fixed`), it is
/// as tall as on the design canvas and shows one row of pads; without it,
/// the pads take the rest of the screen.
fn now_playing(
    view: &PerformView,
    grid: PadGrid,
    fixed: bool,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let panel = div()
        .flex()
        .flex_col()
        .gap(px(PANEL_GAP))
        .p(px(PANEL_PADDING))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.));
    let panel = if fixed {
        panel.flex_none().h(px(NOW_PLAYING_HEIGHT))
    } else {
        panel.flex_1().min_h_0()
    };
    panel
        .child(
            div()
                .h(px(TITLE_ROW_HEIGHT))
                .flex_none()
                .flex()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap(px(6.))
                        // The title's box holds its descenders: a line of 1.15
                        // em is shorter than the font's ascent and descent.
                        .child(
                            div()
                                .text_size(px(26.))
                                .line_height(relative(1.15))
                                .font_weight(FontWeight::BOLD)
                                .child(one_line_text(view.title.clone())),
                        )
                        .child(song_facts(&view.header)),
                )
                .child(now_card(&view.now))
                .child(next_card(&view.next, cx)),
        )
        .child(song_map(&view.map))
        .child(section_controls(view, cx))
        .child(pads(&view.pads, grid, fixed, cx))
}

/// The song's key, tempo and meter, and the arrangement playing, drawn as
/// the design canvas's dropdown; the app cannot switch arrangements yet,
/// so it picks nothing.
fn song_facts(header: &SongHeader) -> impl IntoElement {
    let fact = |name: &'static str, text: String| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.))
            .whitespace_nowrap()
            .child(icon(name, theme::text_faint(), 15.))
            .child(text)
    };
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(14.))
        .font_family(theme::MONO)
        .text_size(px(14.))
        .text_color(theme::text_soft())
        .when_some(header.key.clone(), |row, key| row.child(fact("note", key)))
        .when_some(header.tempo.clone(), |row, tempo| {
            row.child(fact("metronome", tempo))
        })
        .when_some(header.meter.clone(), |row, meter| {
            row.child(div().flex_none().whitespace_nowrap().child(meter))
        })
        .child(
            div()
                .min_w_0()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(8.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .font_family(theme::UI)
                .text_size(px(13.))
                .text_color(theme::text())
                .child(icon("arrangement", theme::text_faint(), 14.))
                .child(
                    div()
                        .min_w_0()
                        .child(one_line_text(header.arrangement.clone())),
                )
                .child(icon("down", theme::text(), 14.)),
        )
}

/// A section's icon on its color, as the Now and Next cards show it.
fn section_badge(kind: SectionKind, name: &'static str) -> impl IntoElement {
    div()
        .size(px(34.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(9.))
        .bg(theme::section(kind))
        .child(icon(name, theme::on_fill(), 18.))
}

fn now_card(now: &NowCard) -> impl IntoElement {
    div()
        .w(px(230.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .py(px(6.))
        .rounded(px(12.))
        .bg(theme::raised())
        .border_2()
        .border_color(theme::text())
        .child(section_badge(now.kind, now.icon))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(small_heading("Now", theme::text_muted()))
                .child(
                    div()
                        .text_size(px(20.))
                        .line_height(relative(1.15))
                        .font_weight(FontWeight::BOLD)
                        .child(one_line_text(now.name.clone())),
                )
                .child(mono_note(
                    now.bars_left.clone().unwrap_or_else(|| now.detail.clone()),
                )),
        )
}

fn next_card(next: &NextCard, cx: &mut Context<Perform>) -> impl IntoElement {
    let card = div()
        .w(px(310.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .pl(px(12.))
        .pr(px(8.))
        .py(px(6.))
        .rounded(px(12.));
    let card = if next.queued {
        card.border_2()
            .border_dashed()
            .border_color(theme::queued())
            .bg(theme::queued_bg())
    } else {
        card.border_1()
            .border_color(theme::border())
            .bg(theme::panel())
    };
    let (heading_color, heading_icon, name_color) = if next.queued {
        (theme::queued(), "jump", theme::text())
    } else {
        (theme::text_muted(), "arrow-right", theme::text_soft())
    };
    card.child(section_badge(next.kind, next.icon))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(icon(heading_icon, heading_color, 13.))
                        .child(small_heading(next.heading.clone(), heading_color)),
                )
                .child(
                    div()
                        .text_size(px(20.))
                        .line_height(relative(1.15))
                        .font_weight(FontWeight::BOLD)
                        .text_color(name_color)
                        .child(one_line_text(next.name.clone())),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child(one_line_text(
                            next.bars_detail
                                .clone()
                                .unwrap_or_else(|| next.detail.clone()),
                        )),
                ),
        )
        .when(next.queued, |card| {
            card.child(
                square_button("cancel", cx, Intent::Ui(UiAction::CancelJump))
                    .size(px(44.))
                    .bg(theme::queued_bg())
                    .border_color(theme::queued_border())
                    .tooltip(hint("Cancel the queued jump · C"))
                    .child(icon("close", theme::queued(), 18.)),
            )
        })
}

/// The arrangement in play order, sized by length and numbered by bar, with
/// the playhead and the queued jump. The song's waveform is to come: the
/// track under the sections shows their colors.
fn song_map(map: &SongMap) -> impl IntoElement {
    let strip = div()
        .h(px(MAP_BLOCKS_HEIGHT))
        .flex_none()
        .flex()
        .children(map.blocks.iter().map(|block| {
            let color = theme::section(block.kind);
            let cell = div()
                .size_full()
                .overflow_hidden()
                .px(px(5.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .bg(color)
                .text_color(theme::on_fill())
                .font_family(theme::CONDENSED)
                .text_size(px(13.))
                .font_weight(FontWeight::BOLD)
                .whitespace_nowrap()
                .child(block.short.clone());
            let cell = if block.is_now {
                cell.border_2().border_color(theme::text())
            } else if block.is_queued {
                cell.border_2()
                    .border_dashed()
                    .border_color(theme::queued())
            } else {
                cell
            };
            div()
                .w(relative(block.width))
                .h_full()
                .pr(px(2.))
                .child(cell)
        }));
    let track = div()
        .relative()
        .h(px(MAP_TRACK_HEIGHT))
        .flex_none()
        .flex()
        .children(map.blocks.iter().map(|block| {
            let color = theme::section(block.kind);
            div().w(relative(block.width)).h_full().pr(px(2.)).child(
                div()
                    .size_full()
                    .rounded(px(3.))
                    .bg(theme::faded(color, 0.3)),
            )
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(map.playhead))
                .bg(theme::faded(theme::text(), 0.12)),
        )
        .when_some(map.jump.as_ref(), |track, jump| {
            track
                .child(
                    div()
                        .absolute()
                        .top(px(-6.))
                        .bottom(px(-6.))
                        .left(relative(jump.at))
                        .border_l_2()
                        .border_dashed()
                        .border_color(theme::queued()),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(-2.))
                        .left(relative(jump.at))
                        .ml(px(6.))
                        .px(px(6.))
                        .py(px(2.))
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .rounded(px(5.))
                        .bg(theme::queued())
                        .text_color(theme::on_fill())
                        .text_size(px(12.))
                        .font_weight(FontWeight::BOLD)
                        .whitespace_nowrap()
                        .child(icon("jump", theme::on_fill(), 12.))
                        .child(jump.label.clone()),
                )
        })
        .child(
            div()
                .absolute()
                .top(px(-6.))
                .bottom(px(-6.))
                .left(relative(map.playhead))
                .w(px(2.))
                .bg(theme::text()),
        );
    let ruler = div()
        .h(px(MAP_RULER_HEIGHT))
        .flex_none()
        .flex()
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(theme::text_faint())
        .children(map.blocks.iter().map(|block| {
            div()
                .w(relative(block.width))
                .h_full()
                .overflow_hidden()
                .whitespace_nowrap()
                .children(block.ruler.clone())
        }));
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(MAP_GAP))
        .child(strip)
        .child(track)
        .child(ruler)
}

fn section_controls(view: &PerformView, cx: &mut Context<Perform>) -> impl IntoElement {
    let sections = view.pads.len();
    div()
        .h(px(SECTION_CONTROLS_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .text_size(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Sections"),
        )
        .child(mono_note(format!(
            "{sections} section{}",
            if sections == 1 { "" } else { "s" }
        )))
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::text_muted())
                .child("Jump at"),
        )
        .child(
            div()
                .flex()
                .p(px(3.))
                .gap(px(2.))
                .rounded(px(10.))
                .bg(theme::bg())
                .border_1()
                .border_color(theme::border())
                .child(timing_button(
                    "timing-section-end",
                    view.jump_timing,
                    JumpTiming::SectionEnd,
                    "Section end",
                    "section-end",
                    cx,
                ))
                .child(timing_button(
                    "timing-next-bar",
                    view.jump_timing,
                    JumpTiming::NextBar,
                    "Next bar",
                    "next-bar",
                    cx,
                )),
        )
        .child(toggle(
            "loop",
            "Loop section",
            "loop",
            view.live_loop,
            Intent::Ui(UiAction::ToggleLiveLoop),
            "Loop the current section · L",
            cx,
        ))
        .child(toggle(
            "infinite",
            "Infinite",
            "infinite",
            view.infinite,
            Intent::Ui(UiAction::ToggleInfinite),
            "Repeat the current section until you move on · I",
            cx,
        ))
}

/// One side of the jump timing toggle; `current` is the timing in force.
fn timing_button(
    id: &'static str,
    current: JumpTiming,
    timing: JumpTiming,
    label: &'static str,
    icon_name: &'static str,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let selected = current == timing;
    let (bg, fg) = if selected {
        (theme::selected(), theme::text())
    } else {
        (theme::bg(), theme::text_muted())
    };
    button(id, cx, Intent::Ui(UiAction::SetJumpTiming(timing)))
        .h(px(28.))
        .px(px(10.))
        .gap(px(6.))
        .rounded(px(7.))
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .bg(bg)
        .text_color(fg)
        .tooltip(hint(format!("Jump at the {} · N", label.to_lowercase())))
        .child(icon(icon_name, fg, 14.))
        .child(label)
}

fn toggle(
    id: &'static str,
    label: &'static str,
    icon_name: &'static str,
    on: bool,
    intent: Intent,
    tip: &'static str,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let color = if on { theme::accent() } else { theme::text() };
    button(id, cx, intent)
        .h(px(34.))
        .px(px(12.))
        .bg(theme::raised())
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border() })
        .text_color(color)
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .tooltip(hint(tip))
        .child(icon(icon_name, color, 18.))
        .child(label)
}

/// The pads in `grid`'s columns: one row of them when the compact mixer
/// is under them (`fixed`), and the rest scrolls; else as many rows as the
/// screen leaves.
fn pads(pads: &[Pad], grid: PadGrid, fixed: bool, cx: &mut Context<Perform>) -> impl IntoElement {
    let area = div().id("pads");
    let area = if fixed {
        area.flex_none().h(px(PADS_HEIGHT))
    } else {
        area.flex_1().min_h_0()
    };
    area.overflow_y_scroll()
        .grid()
        .grid_cols(u16::try_from(grid.columns).unwrap_or(u16::MAX))
        .gap(px(PAD_GAP))
        .content_start()
        .children(pads.iter().map(|pad| pad_button(pad, grid, cx)))
}

fn pad_button(pad: &Pad, grid: PadGrid, cx: &mut Context<Perform>) -> AnyElement {
    let color = theme::section(pad.kind);
    let compact = grid.compact;
    let id = ElementId::Name(format!("pad-{}", pad.position).into());
    let tip = match pad.key {
        Some(key) => format!("Queue {} · {key}", pad.label),
        None => format!("Queue {}", pad.label),
    };
    let button = div()
        .id(id)
        .relative()
        .h(px(grid.height))
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(12.))
        .bg(theme::raised())
        .cursor_pointer()
        .tooltip(hint(tip))
        .on_click(on_click(
            cx,
            Intent::Ui(UiAction::QueueSection(pad.position)),
        ));
    let button = if pad.is_now {
        button.border_2().border_color(theme::text())
    } else if pad.is_queued {
        button
            .border_2()
            .border_dashed()
            .border_color(theme::queued())
    } else if pad.is_next {
        button
            .border_2()
            .border_color(theme::faded(theme::queued(), 0.5))
    } else {
        button.border_1().border_color(theme::border())
    };
    let badge = |label: &'static str, bg: Rgba| {
        div()
            .absolute()
            .top(px(if compact { 7. } else { 12. }))
            .right(px(6.))
            .px(px(6.))
            .py(px(1.))
            .rounded(px(5.))
            .bg(bg)
            .text_color(theme::on_fill())
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .child(label)
    };
    let (strip, icon_size, abbr_size, name_size) = if compact {
        (4., 14., 18., 11.)
    } else {
        (6., 18., 24., 12.)
    };
    button
        .child(div().h(px(strip)).flex_none().bg(color))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(if compact { 8. } else { 10. }))
                .py(px(if compact { 4. } else { 8. }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(icon(pad.icon, color, icon_size))
                        .child(
                            div()
                                .whitespace_nowrap()
                                .font_family(theme::CONDENSED)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(abbr_size))
                                .line_height(relative(1.))
                                .child(pad.abbr.clone()),
                        ),
                )
                .child(
                    div()
                        .text_size(px(name_size))
                        .line_height(relative(1.2))
                        .text_color(theme::text_muted())
                        .child(one_line_text(pad.label.clone())),
                )
                .when_some(pad.bars.clone().filter(|_| !compact), |column, bars| {
                    column.child(
                        div()
                            .mt(px(4.))
                            .font_family(theme::MONO)
                            .text_size(px(11.))
                            .text_color(theme::text_faint())
                            .whitespace_nowrap()
                            .child(bars),
                    )
                }),
        )
        .when(pad.is_now, |button| {
            button.child(badge("Now", theme::text()))
        })
        .when(pad.is_next, |button| {
            button.child(badge("Next", theme::queued()))
        })
        .into_any_element()
}

/// A strip tile being dragged by the pointer.
pub(crate) struct DraggedTile {
    index: usize,
    label: SharedString,
}

/// The line that shows where a dragged tile would land: in the gap before a
/// tile, or after the last one.
fn tile_insertion_line() -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(3.))
        .rounded(px(2.))
        .bg(theme::accent())
}

/// The setlist strip: a tile per item, the chip with its transition
/// between tiles; a tile selects its item, and dragging it along the strip
/// moves it (L46). The "+" after the tiles neither drags nor takes a drop.
/// Tiles share the strip's width, never narrower than the design canvas's
/// 196 points; past that the strip scrolls.
fn strip(setlist: &SetlistView, cx: &mut Context<Perform>) -> impl IntoElement {
    let insertion = setlist.insertion.filter(|_| cx.has_active_drag());
    let tiles = setlist.items.len();
    div()
        .id("strip")
        .h(px(STRIP_HEIGHT))
        .flex_none()
        .flex()
        .gap(px(8.))
        .pb(px(6.))
        .overflow_x_scroll()
        .child(
            div()
                .flex_grow()
                .flex_shrink_0()
                .flex()
                .gap(px(8.))
                // Room for the line before the first tile.
                .pl(px(6.))
                .on_drag_move(
                    cx.listener(|screen, event: &DragMoveEvent<DraggedTile>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            screen.strip_drag_left(cx);
                        }
                    }),
                )
                .on_drop(cx.listener(|screen, tile: &DraggedTile, _, cx| {
                    screen.strip_drop(tile.index, cx);
                }))
                .children(setlist.items.iter().map(|item| {
                    div()
                        .flex_grow()
                        .flex_shrink_0()
                        .flex()
                        .gap(px(8.))
                        .child(strip_tile(item, tiles, insertion, cx))
                        .children(item.transition.as_ref().map(transition_chip))
                })),
        )
        .child(
            button("add-to-setlist", cx, Intent::ShowPage(Page::Library))
                .w(px(52.))
                .justify_center()
                .rounded(px(12.))
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .tooltip(hint("Add songs from the library"))
                .child(icon("plus", theme::text_muted(), 22.)),
        )
}

fn strip_tile(
    item: &StripItem,
    tiles: usize,
    insertion: Option<usize>,
    cx: &mut Context<Perform>,
) -> AnyElement {
    let index = item.index;
    let (border, bg, fg) = match item.state {
        ItemState::Current => (theme::accent(), theme::raised(), theme::text()),
        ItemState::Played => (theme::border(), theme::panel(), theme::text_muted()),
        ItemState::Upcoming => (theme::border(), theme::surface(), theme::text()),
    };
    let (badge_icon, badge_bg, badge_fg) = if !item.playable {
        ("panic", theme::danger_bg(), theme::danger())
    } else {
        match item.state {
            ItemState::Current => ("play", theme::accent(), theme::on_fill()),
            ItemState::Played => ("check", theme::raised(), theme::text_faint()),
            ItemState::Upcoming if item.click => ("metronome", theme::badge(), theme::text_soft()),
            ItemState::Upcoming => ("note", theme::badge(), theme::text_soft()),
        }
    };
    let tag = if item.loading {
        Some(("Loading…", theme::queued()))
    } else if item.is_next {
        Some(("Next", theme::accent()))
    } else {
        None
    };
    let (detail, detail_color) = match &item.warning {
        Some(warning) => (warning.clone(), theme::danger()),
        None => (item.meta.clone(), theme::text_muted()),
    };
    let tile = div()
        .id(ElementId::Name(format!("item-{}", item.index).into()))
        .flex_grow()
        .flex_shrink_0()
        .flex_basis(px(196.))
        .min_w(px(196.))
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .rounded(px(12.))
        .bg(bg)
        .text_color(fg)
        .relative()
        .cursor_pointer()
        .on_click(on_click(cx, Intent::Ui(UiAction::SelectItem(index))))
        .on_drag(
            DraggedTile {
                index,
                label: item.title.clone().into(),
            },
            {
                let screen = cx.entity();
                move |tile: &DraggedTile, _, _, cx: &mut App| {
                    screen.update(cx, |screen, cx| screen.strip_drag_started(cx));
                    drag_preview(&tile.label, cx)
                }
            },
        )
        .on_drag_move(
            cx.listener(move |screen, event: &DragMoveEvent<DraggedTile>, _, cx| {
                if let Some(half) = half_across(&event.bounds, event.event.position) {
                    let from = event.drag(cx).index;
                    screen.strip_drag_over(from, index, half, cx);
                }
            }),
        )
        .when(insertion == Some(index), |tile| {
            tile.child(tile_insertion_line().left(px(-6.)))
        })
        .when(index + 1 == tiles && insertion == Some(tiles), |tile| {
            tile.child(tile_insertion_line().right(px(-6.)))
        });
    let tile = if item.state == ItemState::Current {
        tile.border_2().border_color(border)
    } else {
        tile.border_1().border_color(border)
    };
    tile.child(
        div()
            .size(px(34.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(9.))
            .bg(badge_bg)
            .child(icon(badge_icon, badge_fg, 18.)),
    )
    // The title and meta take the room the badge leaves and add none of
    // their own to the tile: as the design's `flex: 1 0 196px` tile, it
    // keeps its share of the strip and cuts a long title. taffy counts a
    // flex item that clips its content as its basis (0) when it works out
    // the tile's max-content width, which the strip's rows ask for.
    .child(
        div()
            .flex_grow()
            .flex_basis(px(0.))
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::MONO)
                            .text_size(px(12.))
                            .text_color(theme::text_faint())
                            .child(item.number.clone()),
                    )
                    // The title takes its own width and gives way to the
                    // number and tag, ending in an ellipsis
                    // ([`OneLineText`]).
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(one_line_text(item.title.clone())),
                    )
                    .when_some(tag, |row, (tag, color)| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(color)
                                .child(tag),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(detail_color)
                    .child(one_line_text(detail)),
            ),
    )
    .into_any_element()
}

/// How an item leads to the next one.
fn transition_chip(chip: &TransitionChip) -> impl IntoElement {
    let name = match chip.transition {
        Transition::CueNext => "cue",
        Transition::Stay => "stay",
        Transition::AutoLink => "link",
        Transition::Crossfade => "crossfade",
    };
    div()
        .w(px(58.))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .rounded(px(10.))
        .border_1()
        .border_dashed()
        .border_color(theme::chip_border())
        .text_color(theme::text_muted())
        .child(icon(name, theme::text_muted(), 20.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(chip.short),
        )
}

/// One line of the notes under the strip, in its color.
struct Note {
    text: String,
    color: Rgba,
    strong: bool,
}

/// What needs a look, one line each: a crossfade under way and the
/// setlist's problems; the device that does not play, the last problem,
/// the song package's warnings, and why the song header misses its key,
/// tempo and meter. Nothing when all is well, as on the design canvas.
fn notes(view: &PerformView, setlist: Option<&SetlistView>) -> Vec<Note> {
    let note = |text: String, color: Rgba, strong: bool| Note {
        text,
        color,
        strong,
    };
    let mut notes = Vec::new();
    if let Some(setlist) = setlist {
        notes.extend(
            setlist
                .blend
                .clone()
                .map(|blend| note(blend, theme::accent(), true)),
        );
        notes.extend(
            setlist
                .problems
                .iter()
                .map(|line| note(line.clone(), theme::queued(), false)),
        );
    }
    let status = &view.status;
    notes.extend(
        status
            .device_problem
            .iter()
            .map(|problem| note(problem.clone(), theme::danger(), true)),
    );
    notes.extend(
        status
            .last_error
            .iter()
            .map(|error| note(format!("Last problem: {error}"), theme::danger(), true)),
    );
    notes.extend(
        status
            .warnings
            .iter()
            .map(|warning| note(format!("Warning: {warning}"), theme::text_muted(), false)),
    );
    notes.extend(
        status
            .facts_problem
            .iter()
            .map(|problem| note(problem.clone(), theme::queued(), false)),
    );
    notes
}

/// The notes under the strip ([`notes`]).
fn notes_panel(notes: &[Note]) -> impl IntoElement {
    div()
        .id("notes")
        .flex_none()
        .max_h(px(NOTES_MAX_HEIGHT))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(NOTE_GAP))
        .px(px(14.))
        .py(px(NOTES_PADDING_Y))
        .rounded(px(12.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .text_size(px(NOTE_TEXT_SIZE))
        .line_height(px(NOTE_LINE_HEIGHT))
        .children(notes.iter().map(|note| {
            div()
                .text_color(note.color)
                .when(note.strong, |line| line.font_weight(FontWeight::SEMIBOLD))
                .child(note.text.clone())
        }))
}

/// A control's hint while the pointer rests on it: what it does and its
/// key.
struct Hint(SharedString);

impl Render for Hint {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .shadow_md()
            .font_family(theme::UI)
            .text_size(px(12.))
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

/// Builds the [`Hint`] that says `text`, for an element's `tooltip`.
pub(crate) fn hint(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Hint(text.clone())).into()
}

/// A clickable row of an icon and a label that reports `intent`.
pub(crate) fn button(
    id: impl Into<ElementId>,
    cx: &mut Context<Perform>,
    intent: Intent,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .rounded(px(10.))
        .cursor_pointer()
        .on_click(on_click(cx, intent))
}

/// A 48 px square icon button.
pub(crate) fn square_button(
    id: impl Into<ElementId>,
    cx: &mut Context<Perform>,
    intent: Intent,
) -> Stateful<Div> {
    button(id, cx, intent)
        .size(px(SQUARE_BUTTON_SIZE))
        .justify_center()
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
}

/// A bordered button in the header.
pub(crate) fn chip_button(
    id: impl Into<ElementId>,
    cx: &mut Context<Perform>,
    intent: Intent,
) -> Stateful<Div> {
    button(id, cx, intent)
        .h(px(48.))
        .px(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .font_weight(FontWeight::SEMIBOLD)
}

pub(crate) fn on_click(
    cx: &mut Context<Perform>,
    intent: Intent,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |screen, _, window, cx| screen.handle(intent, window, cx))
}

/// The OpenVamp mark and name: the loop in the brand color with the play
/// shape inside, drawn as two tinted icons because GPUI tints a whole SVG.
fn brand_mark() -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .relative()
                .size(px(28.))
                .child(
                    div()
                        .absolute()
                        .child(icon("openvamp-loop", theme::brand(), 28.)),
                )
                .child(
                    div()
                        .absolute()
                        .child(icon("openvamp-play", theme::text(), 28.)),
                ),
        )
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("OpenVamp"),
        )
}

/// What follows the pointer while a row or tile is dragged: its title,
/// compactly.
pub(crate) struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(12.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::accent())
            .shadow_md()
            .text_size(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text())
            .child(self.label.clone())
    }
}

/// The view that follows the pointer while something titled `label` is
/// dragged.
pub(crate) fn drag_preview(label: &SharedString, cx: &mut App) -> Entity<DragPreview> {
    let label = label.clone();
    cx.new(|_| DragPreview { label })
}

/// Which half of the row with `bounds` the pointer at `position` is in,
/// top or bottom, when it is over that row at all.
pub(crate) fn half_under(bounds: &Bounds<Pixels>, position: Point<Pixels>) -> Option<Half> {
    bounds.contains(&position).then(|| {
        half_at(
            f32::from(bounds.origin.y),
            f32::from(bounds.size.height),
            f32::from(position.y),
        )
    })
}

/// Which half of the tile with `bounds` the pointer at `position` is in,
/// left ([`Half::Upper`], before it) or right, when it is over that tile at
/// all.
pub(crate) fn half_across(bounds: &Bounds<Pixels>, position: Point<Pixels>) -> Option<Half> {
    bounds.contains(&position).then(|| {
        half_at(
            f32::from(bounds.origin.x),
            f32::from(bounds.size.width),
            f32::from(position.x),
        )
    })
}

/// One line of text cut with an ellipsis ("…") when it does not fit.
///
/// gpui 0.2.2's own `truncate()` sets `whitespace_nowrap`, and a text that
/// does not wrap keeps the size of its first measurement, taken while its
/// width is not known yet (the layout's max-content pass): it then draws in
/// full and its box clips it, with no ellipsis. A text that wraps is
/// measured again with each width it is asked about, so this one wraps, is
/// clamped to one line, and ends in an ellipsis at the width it was last
/// measured with, which is its box only when no other width came after it:
/// [`OneLineText`] does not depend on that.
pub(crate) trait OneLine: Styled + Sized {
    fn one_line(self) -> Self {
        self.whitespace_normal().text_ellipsis().line_clamp(1)
    }
}

impl<E: Styled> OneLine for E {}

/// One line of text, in the text style its parents set, that ends in an
/// ellipsis ("…") when its box is narrower than the text. Its box is as
/// wide as the whole text unless its parent makes it narrower (a block
/// parent stretches it to its own width, and as a flex item it shrinks to
/// nothing before it overflows), and never shorter than the font's ascent
/// and descent, so its glyphs stay inside it.
///
/// [`OneLine`] is not enough where the text's width comes from the layout
/// rather than from the text: gpui 0.2.2's text measures itself with each
/// width taffy asks about, keeps only the last result, gives that result
/// again for any question without a width (its min- and max-content), and
/// paints those lines (`TextLayout::layout` in gpui's `elements/text.rs`).
/// Taffy asks a flex item about several widths and answers repeated
/// questions from its own cache without asking the text again, so the
/// width the text was last cut to need not be the box it ends up in: cut
/// to nothing in a narrow pass, a title then says its width is nothing and
/// stays gone; cut to three letters, it paints three letters in a wide box.
/// This text answers every question with its whole width, and cuts itself
/// to its final box when it paints.
pub(crate) struct OneLineText {
    text: SharedString,
}

pub(crate) fn one_line_text(text: impl Into<SharedString>) -> OneLineText {
    let text: SharedString = text.into();
    // A shaped line holds no line breaks.
    let text = if text.contains('\n') {
        text.replace('\n', " ").into()
    } else {
        text
    };
    OneLineText { text }
}

/// The whole text shaped in its style, and its line box's height.
pub(crate) struct OneLineLayout {
    line: ShapedLine,
    height: Pixels,
}

impl OneLineText {
    fn run(&self, window: &Window) -> (TextRun, Pixels) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        (style.to_run(self.text.len()), font_size)
    }
}

impl IntoElement for OneLineText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for OneLineText {
    type RequestLayoutState = OneLineLayout;
    /// The text cut to its box, when the whole text does not fit.
    type PrepaintState = Option<ShapedLine>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (run, font_size) = self.run(window);
        let line_height = window
            .text_style()
            .line_height
            .to_pixels(font_size.into(), window.rem_size());
        let line = window
            .text_system()
            .shape_line(self.text.clone(), font_size, &[run], None);
        let height = line_height.max(line.ascent + line.descent);
        let width = line.width.ceil();
        let mut style = Style::default();
        style.min_size.width = px(0.).into();
        let layout_id = window.request_measured_layout(style, move |known, _, _, _| Size {
            width: known.width.unwrap_or(width),
            height,
        });
        (layout_id, OneLineLayout { line, height })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if layout.line.width <= bounds.size.width {
            return None;
        }
        let (run, font_size) = self.run(window);
        let font = run.font.clone();
        let mut runs = vec![run];
        let cut = cx
            .text_system()
            .line_wrapper(font, font_size)
            .truncate_line(self.text.clone(), bounds.size.width, "…", &mut runs);
        Some(window.text_system().shape_line(cut, font_size, &runs, None))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        cut: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line = cut.as_ref().unwrap_or(&layout.line);
        // Painting fails only when a glyph cannot be rasterized; the line
        // then shows without it, as gpui's own text does.
        let _ = line.paint(bounds.origin, layout.height, window, cx);
    }
}

pub(crate) fn icon(name: &str, color: Rgba, size: f32) -> impl IntoElement {
    svg()
        .path(theme::icon_path(name))
        .flex_none()
        .size(px(size))
        .text_color(color)
}

pub(crate) fn small_heading(text: impl Into<SharedString>, color: Rgba) -> impl IntoElement {
    div()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(text.into())
}

fn mono_note(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .font_family(theme::MONO)
        .text_size(px(12.))
        .text_color(theme::text_muted())
        .whitespace_nowrap()
        .child(text.into())
}
