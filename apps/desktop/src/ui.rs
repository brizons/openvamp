//! The Perform screen's presentational code: draws a [`PerformView`], and
//! for a setlist a [`SetlistView`], after the design canvas's Main artboard
//! (reduced to S6 and S4), and reports what the person does as an
//! [`Intent`]. It decides nothing.

use app::JumpTiming;
use domain::Transition;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, DragMoveEvent, ElementId, Entity,
    FocusHandle, FontWeight, IntoElement, MouseButton, Pixels, Point, Rgba, SharedString, Stateful,
    Window, div, prelude::*, px, relative, svg,
};
use openvamp_desktop::action::UiAction;
use openvamp_desktop::reorder::{Half, half_at};
use openvamp_desktop::strip::{ItemState, SetlistView, StripItem, TransitionChip};
use openvamp_desktop::view::{
    FadeButton, Level, NextCard, NowCard, Pad, PerformView, SongMap, StatusView, TransportView,
};

use openvamp_desktop::action::{Intent, Page};

use crate::screen::Perform;
use crate::theme;

/// The keys, as the status area lists them.
const KEYS: &str = if cfg!(target_os = "macos") {
    "Space play/pause · S stop · B start · 1–9 queue · N timing · L loop · I infinite · F fade · C cancel · hold X panic · ⌘O open · ⌘Q quit"
} else {
    "Space play/pause · S stop · B start · 1–9 queue · N timing · L loop · I infinite · F fade · C cancel · hold X panic · Ctrl-O open · Ctrl-Q quit"
};

/// The keys of a setlist, listed before [`KEYS`].
const SETLIST_KEYS: &str = ", previous · . next · ";

/// How the empty screen names the open shortcut.
const OPEN_HINT: &str = if cfg!(target_os = "macos") {
    "⌘O opens a song at any time"
} else {
    "Ctrl-O opens a song at any time"
};

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
        .gap(px(12.))
        .p(px(16.))
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
        .w(px(64.))
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
/// not stop it.
pub fn perform(
    view: &PerformView,
    setlist: Option<&SetlistView>,
    hold: f32,
    problems: &[String],
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(header(view, setlist, hold, cx))
        .when(!problems.is_empty(), |screen| {
            screen.child(problem_banner(problems, cx))
        })
        .when_some(setlist, |screen, setlist| {
            screen.child(strip(setlist, cx)).when(
                !setlist.problems.is_empty() || setlist.blend.is_some(),
                |screen| screen.child(setlist_notes(setlist)),
            )
        })
        .child(now_playing(view, cx))
        .child(status(&view.status, setlist.is_some()))
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
        .py(px(10.))
        .bg(theme::danger_bg())
        .border_1()
        .border_color(theme::danger())
        .rounded(px(14.))
        .text_size(px(13.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
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

fn header(
    view: &PerformView,
    setlist: Option<&SetlistView>,
    hold: f32,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let header = div()
        .h(px(64.))
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
                    .child(icon("folder", theme::text_muted(), 20.))
                    .child("Open song…"),
            )
            .child(
                chip_button("open-setlist", cx, Intent::OpenSetlist)
                    .child(icon("setlist", theme::text_muted(), 20.))
                    .child("Open setlist…"),
            ),
    };
    header
        .child(transport(&view.transport, setlist, cx))
        .child(
            div()
                .flex_none()
                .min_w(px(150.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(22.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(view.clock.clone()),
                )
                .child(
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child(format!("{} · {}", view.bar_beat, view.transport.state)),
                ),
        )
        .child(div().flex_1())
        .child(fade(&view.fade, cx))
        .child(panic(hold, cx))
}

/// The setlist's name and the current item's place; picks another
/// setlist.
fn setlist_chip(setlist: &SetlistView, cx: &mut Context<Perform>) -> impl IntoElement {
    // It gives way to the transport and the clock, but never below a width
    // that keeps the start of the name readable.
    chip_button("setlist", cx, Intent::OpenSetlist)
        .flex_shrink()
        .min_w(px(140.))
        .max_w(px(260.))
        .child(icon("setlist", theme::text_muted(), 20.))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().one_line().child(setlist.name.clone()))
                .child(
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(11.))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::text_muted())
                        .child(setlist.position.clone()),
                ),
        )
}

/// Back to start, play/pause and stop; with a setlist, previous and next
/// song around play, as on the design canvas.
fn transport(
    transport: &TransportView,
    setlist: Option<&SetlistView>,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let (play_icon, play_label) = if transport.playing {
        ("pause", "Pause")
    } else {
        ("play", "Play")
    };
    let step = |name: &'static str, action: UiAction, enabled: bool, cx: &mut Context<Perform>| {
        let color = if enabled {
            theme::text()
        } else {
            theme::text_faint()
        };
        square_button(name, cx, Intent::Ui(action)).child(icon(name, color, 22.))
    };
    let previous =
        setlist.map(|setlist| step("previous", UiAction::PreviousSong, setlist.has_previous, cx));
    let next = setlist.map(|setlist| step("next", UiAction::NextSong, setlist.has_next, cx));
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            square_button("back", cx, Intent::Ui(UiAction::BackToStart)).child(icon(
                "back-to-start",
                theme::text(),
                22.,
            )),
        )
        .children(previous)
        .child(
            button("play", cx, Intent::Ui(UiAction::TogglePlay))
                .w(px(96.))
                .h(px(52.))
                .justify_center()
                .rounded(px(14.))
                .bg(theme::accent())
                .text_color(theme::on_fill())
                .font_weight(FontWeight::BOLD)
                .child(icon(play_icon, theme::on_fill(), 26.))
                .child(play_label),
        )
        .children(next)
        .child(
            square_button("stop", cx, Intent::Ui(UiAction::Stop)).child(icon(
                "stop",
                theme::text(),
                22.,
            )),
        )
}

fn fade(fade: &FadeButton, cx: &mut Context<Perform>) -> impl IntoElement {
    let (bg, fg) = if fade.active {
        (theme::text(), theme::on_fill())
    } else {
        (theme::raised(), theme::text())
    };
    button("fade", cx, Intent::Ui(UiAction::ToggleFade))
        .h(px(48.))
        .min_w(px(150.))
        .px(px(16.))
        .bg(bg)
        .text_color(fg)
        .border_1()
        .border_color(theme::border())
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon("fade", fg, 20.))
        .child(fade.label.clone())
}

/// Panic fires only after a full hold; the fill shows how far it is.
fn panic(hold: f32, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .id("panic")
        .relative()
        .overflow_hidden()
        .h(px(48.))
        .px(px(18.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(10.))
        .border_1()
        .border_color(theme::danger())
        .bg(theme::danger_bg())
        .text_color(theme::danger_text())
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
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

fn now_playing(view: &PerformView, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.))
        .child(
            div()
                .h(px(68.))
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
                        .gap(px(4.))
                        .child(
                            div()
                                .one_line()
                                .text_size(px(26.))
                                .font_weight(FontWeight::BOLD)
                                .child(view.title.clone()),
                        )
                        .child(
                            div()
                                .one_line()
                                .text_size(px(14.))
                                .text_color(theme::text_muted())
                                .child(view.subtitle.clone()),
                        ),
                )
                .child(now_card(&view.now))
                .child(next_card(&view.next, cx)),
        )
        .child(song_map(&view.map))
        .child(section_controls(view, cx))
        .child(pads(&view.pads, cx))
}

fn now_card(now: &NowCard) -> impl IntoElement {
    div()
        .w(px(240.))
        .flex_none()
        .flex()
        .flex_col()
        .justify_center()
        .px(px(12.))
        .rounded(px(12.))
        .bg(theme::raised())
        .border_2()
        .border_color(theme::text())
        .child(small_heading("Now", theme::text_muted()))
        .child(
            div()
                .one_line()
                .text_size(px(20.))
                .font_weight(FontWeight::BOLD)
                .child(now.name.clone()),
        )
        .child(mono_note(now.detail.clone()))
}

fn next_card(next: &NextCard, cx: &mut Context<Perform>) -> impl IntoElement {
    let card = div()
        .w(px(330.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .pl(px(12.))
        .pr(px(8.))
        .rounded(px(12.));
    let card = if next.queued {
        card.border_2()
            .border_dashed()
            .border_color(theme::queued())
            .bg(theme::queued_bg())
    } else {
        card.border_1()
            .border_color(theme::border())
            .bg(theme::bg())
    };
    let heading_color = if next.queued {
        theme::queued()
    } else {
        theme::text_muted()
    };
    card.child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(small_heading(next.heading.clone(), heading_color))
            .child(
                div()
                    .one_line()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(next.name.clone()),
            )
            .child(
                div()
                    .one_line()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(next.detail.clone()),
            ),
    )
    .when(next.queued, |card| {
        card.child(
            square_button("cancel", cx, Intent::Ui(UiAction::CancelJump))
                .bg(theme::queued_bg())
                .border_color(theme::queued_border())
                .child(icon("close", theme::queued(), 18.)),
        )
    })
}

/// The arrangement in play order, sized by length, with the playhead and
/// the queued jump.
fn song_map(map: &SongMap) -> impl IntoElement {
    let strip = div()
        .h(px(30.))
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
        .h(px(40.))
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
                        .top(px(2.))
                        .left(relative(jump.at))
                        .ml(px(6.))
                        .px(px(6.))
                        .py(px(2.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
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
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(strip)
        .child(track)
}

fn section_controls(view: &PerformView, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .h(px(44.))
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
        .child(mono_note(format!("{}", view.pads.len())))
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
            cx,
        ))
        .child(toggle(
            "infinite",
            "Infinite",
            "infinite",
            view.infinite,
            Intent::Ui(UiAction::ToggleInfinite),
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
        .h(px(36.))
        .px(px(12.))
        .rounded(px(7.))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .bg(bg)
        .text_color(fg)
        .child(icon(icon_name, fg, 16.))
        .child(label)
}

fn toggle(
    id: &'static str,
    label: &'static str,
    icon_name: &'static str,
    on: bool,
    intent: Intent,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let color = if on { theme::accent() } else { theme::text() };
    button(id, cx, intent)
        .h(px(44.))
        .px(px(14.))
        .bg(theme::raised())
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border() })
        .text_color(color)
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon(icon_name, color, 18.))
        .child(label)
}

fn pads(pads: &[Pad], cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .id("pads")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_wrap()
        .content_start()
        .gap(px(8.))
        .children(pads.iter().map(|pad| pad_button(pad, cx)))
}

fn pad_button(pad: &Pad, cx: &mut Context<Perform>) -> AnyElement {
    let color = theme::section(pad.kind);
    let id = ElementId::Name(format!("pad-{}", pad.position).into());
    let button = div()
        .id(id)
        .relative()
        .w(px(156.))
        .h(px(100.))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(12.))
        .bg(theme::raised())
        .cursor_pointer()
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
            .px(px(6.))
            .py(px(1.))
            .rounded(px(5.))
            .bg(bg)
            .text_color(theme::on_fill())
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .child(label)
    };
    button
        .child(div().h(px(6.)).flex_none().bg(color))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_between()
                .p(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .when_some(pad.key, |row, key| {
                            row.child(
                                div()
                                    .font_family(theme::MONO)
                                    .text_size(px(12.))
                                    .text_color(theme::text_faint())
                                    .child(key.to_string()),
                            )
                        })
                        .child(div().flex_1())
                        .when(pad.is_now, |row| row.child(badge("Now", theme::text())))
                        .when(pad.is_next, |row| row.child(badge("Next", theme::queued()))),
                )
                .child(
                    div()
                        .one_line()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .child(pad.label.clone()),
                ),
        )
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
fn strip(setlist: &SetlistView, cx: &mut Context<Perform>) -> impl IntoElement {
    let insertion = setlist.insertion.filter(|_| cx.has_active_drag());
    let tiles = setlist.items.len();
    div()
        .id("strip")
        .h(px(84.))
        .flex_none()
        .flex()
        .gap(px(8.))
        .pb(px(6.))
        .overflow_x_scroll()
        .child(
            div()
                .flex_none()
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
                        .flex_none()
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
        .w(px(240.))
        .flex_none()
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
    .child(
        div()
            .flex_1()
            .min_w_0()
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
                    // The title takes the room the number and tag leave: a
                    // width from its own measurement could stay at the one
                    // gpui 0.2.2 took first, before the tile had a width.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .one_line()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(item.title.clone()),
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
                    .one_line()
                    .text_size(px(13.))
                    .text_color(detail_color)
                    .child(detail),
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

/// A crossfade under way and the setlist's problems, under the strip.
fn setlist_notes(setlist: &SetlistView) -> impl IntoElement {
    div()
        .id("setlist-notes")
        .flex_none()
        .max_h(px(110.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(14.))
        .py(px(8.))
        .rounded(px(12.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .when_some(setlist.blend.clone(), |notes, blend| {
            notes.child(
                div()
                    .text_color(theme::accent())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(blend),
            )
        })
        .children(
            setlist
                .problems
                .iter()
                .map(|line| div().text_color(theme::queued()).child(line.clone())),
        )
}

fn status(status: &StatusView, setlist: bool) -> impl IntoElement {
    let dot = match status.device_level {
        Level::Ok => theme::accent(),
        Level::Error => theme::danger(),
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(14.))
        .py(px(10.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.))
        .text_size(px(13.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .child(icon("device", theme::text_muted(), 18.))
                .child(div().size(px(8.)).rounded(px(4.)).bg(dot))
                // The device keeps a readable width; the long key list
                // gives way first.
                .child(
                    div()
                        .flex_shrink()
                        .min_w(px(160.))
                        .one_line()
                        .child(status.device.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(theme::MONO)
                        .text_color(theme::text_muted())
                        .child(status.underruns.clone()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .min_w_0()
                        .one_line()
                        .text_size(px(12.))
                        .text_color(theme::text_faint())
                        .child(if setlist {
                            format!("{SETLIST_KEYS}{KEYS}")
                        } else {
                            KEYS.to_owned()
                        }),
                ),
        )
        .when_some(status.last_error.clone(), |area, error| {
            area.child(
                div()
                    .text_color(theme::danger())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Last problem: {error}")),
            )
        })
        .children(status.warnings.iter().map(|warning| {
            div()
                .text_color(theme::text_muted())
                .child(format!("Warning: {warning}"))
        }))
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
        .size(px(48.))
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
/// measured again once its width is known, so this one wraps, is clamped
/// to one line, and ends in an ellipsis at that width.
pub(crate) trait OneLine: Styled + Sized {
    fn one_line(self) -> Self {
        self.whitespace_normal().text_ellipsis().line_clamp(1)
    }
}

impl<E: Styled> OneLine for E {}

pub(crate) fn icon(name: &str, color: Rgba, size: f32) -> impl IntoElement {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
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
        .child(text.into())
}
