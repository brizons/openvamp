//! The Library & setlists screen's presentational code: draws a
//! [`LibraryView`] after the design canvas's Library artboard (the setlists,
//! the setlist editor and the songs) and reports what the person does as an
//! [`Intent`]. It decides nothing.

use domain::Transition;
use gpui::{
    AnyElement, App, Context, Div, DragMoveEvent, ElementId, Entity, FontWeight, IntoElement,
    SharedString, Stateful, div, prelude::*, px,
};
use openvamp_desktop::action::Intent;
use openvamp_desktop::catalog::{TRANSITIONS, transition_name};
use openvamp_desktop::library_page::{
    ClickDetails, Dragged, EditorRow, EditorView, LibraryAction, LibraryView, RowDetails,
    SetlistRow, SongDetails, SongRow, UnreadableRow,
};

use crate::screen::Perform;
use crate::theme;
use crate::ui::{DragPreview, OneLine, button, chip_button, drag_preview, half_under, icon};

/// An element id made of words and numbers, such as `arrangement-2-1`.
fn id(name: String) -> ElementId {
    ElementId::Name(name.into())
}

/// An item of the editor being dragged by the pointer.
pub(crate) struct DraggedItem {
    index: usize,
    label: SharedString,
}

/// A setlist of the list being dragged by the pointer.
pub(crate) struct DraggedSetlist {
    index: usize,
    label: SharedString,
}

/// Starts a drag on `screen` and builds what follows the pointer: the
/// dragged row's `label`.
fn start_drag(screen: &Entity<Perform>, label: &SharedString, cx: &mut App) -> Entity<DragPreview> {
    screen.update(cx, |screen, cx| screen.library_drag_started(cx));
    drag_preview(label, cx)
}

/// The line that shows where a dragged row would land: in the gap above
/// a row, or below the last one.
fn insertion_line() -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(3.))
        .rounded(px(2.))
        .bg(theme::accent())
}

fn act(action: LibraryAction) -> Intent {
    Intent::Library(action)
}

/// The Library screen before a library folder is open.
pub fn no_library(problem: Option<&str>, cx: &mut Context<Perform>) -> impl IntoElement {
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
                .child("No library open"),
        )
        .child(div().text_color(theme::text_muted()).child(
            "Open a library folder: its song folders, and its Setlists folder for the setlists.",
        ))
        .children(problem.map(|problem| {
            div()
                .max_w(px(900.))
                .text_color(theme::danger())
                .child(problem.to_owned())
        }))
        .child(
            button("open-library-empty", cx, Intent::OpenLibrary)
                .h(px(48.))
                .px(px(18.))
                .bg(theme::text())
                .text_color(theme::on_fill())
                .font_weight(FontWeight::BOLD)
                .child(icon("folder", theme::on_fill(), 20.))
                .child("Open library…"),
        )
}

/// The Library screen: the setlists, the editor and the songs.
pub fn library(view: &LibraryView, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(12.))
        .when_some(view.notice.clone(), |page, notice| {
            page.child(notice_banner(notice, cx))
        })
        .when_some(view.warning.clone(), |page, warning| {
            page.child(warning_line(warning, cx))
        })
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(12.))
                .child(setlists(view, cx))
                .child(editor(view.editor.as_ref(), cx))
                .child(songs(view, cx)),
        )
}

fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.))
}

fn notice_banner(notice: String, cx: &mut Context<Perform>) -> impl IntoElement {
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
                .text_color(theme::danger_text())
                .child(notice),
        )
        .child(
            small_square("dismiss-notice", cx, act(LibraryAction::DismissNotice)).child(icon(
                "close",
                theme::danger_text(),
                18.,
            )),
        )
}

/// Something done that is not quite whole, such as a setlist saved
/// without its place in the stored order: a warning, not a failure.
fn warning_line(warning: String, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .py(px(8.))
        .bg(theme::queued_bg())
        .border_1()
        .border_color(theme::queued_border())
        .rounded(px(14.))
        .text_size(px(13.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme::queued())
                .child(warning),
        )
        .child(
            small_square("dismiss-warning", cx, act(LibraryAction::DismissNotice)).child(icon(
                "close",
                theme::queued(),
                18.,
            )),
        )
}

/// Removes the setlist in two presses: the first arms it and says so, the
/// second moves its file to the system trash.
fn remove_button(view: &EditorView, cx: &mut Context<Perform>) -> impl IntoElement {
    let (bg, fg, border) = if view.remove_armed {
        (theme::danger_bg(), theme::danger_text(), theme::danger())
    } else {
        (theme::raised(), theme::text(), theme::border())
    };
    // It shrinks, down to its icon, well before the setlist's name does.
    gives_way_first(button(
        "remove-setlist",
        cx,
        act(LibraryAction::RemoveSetlist),
    ))
    .min_w(px(36.))
    .h(px(36.))
    .px(px(10.))
    .rounded(px(8.))
    .bg(bg)
    .border_1()
    .border_color(border)
    .text_color(fg)
    .text_size(px(13.))
    .font_weight(FontWeight::SEMIBOLD)
    .child(icon("trash", fg, 18.))
    .child(div().min_w_0().one_line().child(view.remove.clone()))
}

/// How much faster an element that gives way first shrinks than its
/// neighbours: flex shrinking is weighted by this factor and the element's
/// width, so it loses most of a shortfall until it reaches its minimum.
const GIVES_WAY: f32 = 20.;

/// `element`, shrinking [`GIVES_WAY`] times faster than its neighbours.
fn gives_way_first<E: Styled>(mut element: E) -> E {
    element.style().flex_shrink = Some(GIVES_WAY);
    element
}

fn heading(name: &'static str, title: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(22.))
        .font_weight(FontWeight::BOLD)
        .child(icon(name, theme::text_muted(), 20.))
        .child(title)
}

/// A muted line of `size` px led by an icon, as the design writes a
/// setlist's date and length: here the editor's on-Perform line and its
/// summary.
fn meta_line(name: &'static str, icon_color: gpui::Rgba, text: String, size: f32) -> Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(size))
        .text_color(theme::text_muted())
        .child(icon(name, icon_color, size + 1.))
        .child(div().min_w_0().one_line().child(text))
}

/// The setlists of the library, the one in the editor marked.
fn setlists(view: &LibraryView, cx: &mut Context<Perform>) -> impl IntoElement {
    let insertion = view.setlist_insertion.filter(|_| cx.has_active_drag());
    panel()
        .w(px(280.))
        .flex_none()
        .child(
            div()
                .flex()
                .items_center()
                .child(heading("setlist", "Setlists"))
                .child(div().flex_1())
                .child(
                    button("new-setlist", cx, act(LibraryAction::NewSetlist))
                        .h(px(36.))
                        .pl(px(10.))
                        .pr(px(12.))
                        .gap(px(6.))
                        .rounded(px(8.))
                        .bg(theme::text())
                        .text_color(theme::on_fill())
                        .text_size(px(14.))
                        .font_weight(FontWeight::BOLD)
                        .child(icon("plus", theme::on_fill(), 16.))
                        .child("New"),
                ),
        )
        .child(
            div()
                .id("setlists")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(8.))
                .py(px(6.))
                .on_drag_move(cx.listener(
                    |screen, event: &DragMoveEvent<DraggedSetlist>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            screen.library_drag_left(cx);
                        }
                    },
                ))
                .on_drop(cx.listener(|screen, setlist: &DraggedSetlist, _, cx| {
                    screen.library_drop(Dragged::Setlist(setlist.index), cx);
                }))
                .children(
                    view.setlists
                        .iter()
                        .map(|row| setlist_row(row, view.setlists.len(), insertion, cx)),
                )
                .children(
                    view.order_problem
                        .iter()
                        .chain(&view.entry_problems)
                        .map(|line| {
                            div()
                                .text_size(px(12.))
                                .text_color(theme::queued())
                                .child(line.clone())
                        }),
                )
                .children(
                    view.unreadable_setlists
                        .iter()
                        .map(|row| unreadable_row(row, cx)),
                ),
        )
        .child(
            div()
                .one_line()
                .text_size(px(12.))
                .text_color(theme::text_faint())
                .child(view.folder.clone()),
        )
        .child(
            chip_button("open-library", cx, Intent::OpenLibrary)
                .h(px(40.))
                .child(icon("folder", theme::text_muted(), 18.))
                .child("Open library…"),
        )
}

/// A setlist of the list: the arrows move it, the rest selects it.
fn setlist_row(
    row: &SetlistRow,
    rows: usize,
    insertion: Option<usize>,
    cx: &mut Context<Perform>,
) -> AnyElement {
    let index = row.index;
    let step = |name: &'static str, enabled: bool, action, cx: &mut Context<Perform>| {
        let color = if enabled {
            theme::text_muted()
        } else {
            theme::border()
        };
        button(id(format!("setlist-{name}-{index}")), cx, act(action))
            .size(px(24.))
            .justify_center()
            .rounded(px(6.))
            .child(icon(name, color, 16.))
    };
    let tile = button(
        ("setlist-row", row.index),
        cx,
        act(LibraryAction::SelectSetlist(row.index)),
    )
    .flex_1()
    .min_w_0()
    .flex_col()
    .items_start()
    .gap(px(3.))
    .p(px(12.));
    let tile = if row.selected {
        tile.border_2()
            .border_color(theme::text())
            .bg(theme::raised())
    } else {
        tile.border_1()
            .border_color(theme::border())
            .bg(theme::panel())
    };
    let meta = if row.playing {
        format!("{} · on Perform", row.meta)
    } else {
        row.meta.clone()
    };
    let tile = tile
        .child(
            div()
                .w_full()
                .one_line()
                .text_size(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(row.name.clone()),
        )
        .child(
            div()
                .w_full()
                .one_line()
                .text_size(px(13.))
                .text_color(theme::text_muted())
                .child(meta),
        );
    // The whole row drags, the grip says so; the arrows stay for the
    // keyboard and as a fallback.
    div()
        .id(("setlist-drag", index))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .cursor_grab()
        .on_drag(
            DraggedSetlist {
                index,
                label: row.name.clone().into(),
            },
            {
                let screen = cx.entity();
                move |dragged: &DraggedSetlist, _, _, cx: &mut App| {
                    start_drag(&screen, &dragged.label, cx)
                }
            },
        )
        .on_drag_move(cx.listener(
            move |screen, event: &DragMoveEvent<DraggedSetlist>, _, cx| {
                if let Some(half) = half_under(&event.bounds, event.event.position) {
                    let from = event.drag(cx).index;
                    screen.library_drag_over(Dragged::Setlist(from), index, half, cx);
                }
            },
        ))
        .when(insertion == Some(index), |row| {
            row.child(insertion_line().top(px(-6.)))
        })
        .when(index + 1 == rows && insertion == Some(rows), |row| {
            row.child(insertion_line().bottom(px(-6.)))
        })
        .child(icon("grip", theme::text_faint(), 14.))
        .child(
            div()
                .flex()
                .flex_col()
                .child(step(
                    "up",
                    index > 0,
                    LibraryAction::MoveSetlistUp(index),
                    cx,
                ))
                .child(step(
                    "down",
                    index + 1 < rows,
                    LibraryAction::MoveSetlistDown(index),
                    cx,
                )),
        )
        .child(tile)
        .into_any_element()
}

/// A setlist file that cannot be read: why, and a two-press remove.
fn unreadable_row(row: &UnreadableRow, cx: &mut Context<Perform>) -> AnyElement {
    let (fg, border) = if row.remove_armed {
        (theme::danger_text(), theme::danger())
    } else {
        (theme::text_muted(), theme::border())
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::danger())
                .child(format!("Cannot read {}", row.line)),
        )
        .child(
            button(
                ("remove-unreadable", row.index),
                cx,
                act(LibraryAction::RemoveUnreadable(row.index)),
            )
            .h(px(28.))
            .px(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .text_size(px(12.))
            .text_color(fg)
            .child(icon("trash", fg, 14.))
            .child(div().min_w_0().one_line().child(row.remove.clone())),
        )
        .into_any_element()
}

/// The setlist in the editor: its name, items and their controls.
fn editor(view: Option<&EditorView>, cx: &mut Context<Perform>) -> impl IntoElement {
    let panel = panel().flex_1().min_w_0().gap(px(12.));
    let Some(view) = view else {
        return panel.items_center().justify_center().child(
            div()
                .text_color(theme::text_muted())
                .child("Pick a setlist, or create one with New."),
        );
    };
    let insertion = view.insertion.filter(|_| cx.has_active_drag());
    let title: AnyElement = match &view.renaming {
        Some(name) => text_field(name, true, "Setlist name", None).into_any_element(),
        None => div()
            .one_line()
            .text_size(px(26.))
            .font_weight(FontWeight::BOLD)
            .child(view.name.clone())
            .into_any_element(),
    };
    panel
        .child(
            div()
                .flex()
                .items_end()
                .gap(px(12.))
                // The name asks for its whole width and takes all the room
                // the buttons leave; the Remove button gives way first, and
                // the name ends in an ellipsis only below that.
                .child(
                    div()
                        .flex_grow()
                        .flex_shrink()
                        .min_w(px(HEADER_NAME_MIN))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        // Where the design writes the setlist's date.
                        .when_some(view.on_perform.clone(), |name, note| {
                            name.child(meta_line("perform", theme::accent(), note, 13.))
                        })
                        .child(title),
                )
                .child(
                    small_square("rename-setlist", cx, act(LibraryAction::RenameSetlist))
                        .child(icon("pencil", theme::text(), 18.)),
                )
                .child(
                    small_square(
                        "duplicate-setlist",
                        cx,
                        act(LibraryAction::DuplicateSetlist),
                    )
                    .child(icon("copy", theme::text(), 18.)),
                )
                .child(remove_button(view, cx))
                .child(
                    meta_line("clock", theme::text_muted(), view.summary.clone(), 14.).mb(px(10.)),
                )
                .child(
                    button("perform-setlist", cx, Intent::PerformSetlist)
                        .h(px(44.))
                        .px(px(16.))
                        .bg(theme::accent())
                        .text_color(theme::on_fill())
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .child(icon("play", theme::on_fill(), 18.))
                        .child("Perform"),
                ),
        )
        .children(view.problems.iter().map(|problem| {
            div()
                .text_size(px(13.))
                .text_color(theme::danger())
                .child(problem.clone())
        }))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .px(px(10.))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(number_column(div()).pl(px(NUMBER_OFFSET)).child("#"))
                .child(song_column(div()).child("Song"))
                .child(arrangement_column(div()).one_line().child("Arrangement"))
                .child(transition_column(div()).one_line().child("Transition out"))
                .child(length_column(div()).text_right().child("Length"))
                .child(remove_column(div())),
        )
        .child(
            div()
                .id("items")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(8.))
                .pt(px(6.))
                .on_drag_move(
                    cx.listener(|screen, event: &DragMoveEvent<DraggedItem>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            screen.library_drag_left(cx);
                        }
                    }),
                )
                .on_drop(cx.listener(|screen, item: &DraggedItem, _, cx| {
                    screen.library_drop(Dragged::Item(item.index), cx);
                }))
                .when(view.rows.is_empty(), |list| {
                    list.child(
                        div()
                            .flex_none()
                            .p(px(28.))
                            .rounded(px(12.))
                            .border_1()
                            .border_dashed()
                            .border_color(theme::chip_border())
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(8.))
                            .child(icon("setlist", theme::text_faint(), 32.))
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("This setlist is empty"),
                            )
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(theme::text_muted())
                                    .child("Add songs from the library on the right."),
                            ),
                    )
                })
                .children(
                    view.rows
                        .iter()
                        .map(|row| editor_row(row, view.rows.len(), insertion, cx)),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(8.))
                        .child(
                            dashed_button("add-song", cx, act(LibraryAction::Search))
                                .child(icon("note", theme::text_muted(), 18.))
                                .child("Add song"),
                        )
                        .child(
                            dashed_button("add-click", cx, act(LibraryAction::AddClick))
                                .child(icon("metronome", theme::text_muted(), 18.))
                                .child("Add click tile"),
                        ),
                ),
        )
}

/// Narrowest the setlist's name in the editor's header gets beside the
/// header's buttons.
const HEADER_NAME_MIN: f32 = 140.;
/// Width of the grip, the move arrows and the number.
const NUMBER_WIDTH: f32 = 64.;
/// Size of an item's grip.
const GRIP_SIZE: f32 = 16.;
/// Width of an item's move arrows.
const ARROWS_WIDTH: f32 = 28.;
/// Gap between an item's grip, arrows and number.
const NUMBER_GAP: f32 = 2.;
/// Where an item's number starts in its column, so the "#" label sits
/// over it.
const NUMBER_OFFSET: f32 = GRIP_SIZE + NUMBER_GAP + ARROWS_WIDTH + NUMBER_GAP;
/// Gap between the columns of the item rows.
const COLUMN_GAP: f32 = 10.;
/// Where an item's problems and controls start: under its song.
const DETAILS_INDENT: f32 = NUMBER_WIDTH + COLUMN_GAP;

// The item rows' columns, shared by the column labels and every row so
// they line up. The song takes the room left and never goes below a width
// that shows a short title in full; the arrangement and the transition
// start at the design's width and give way down to a minimum that keeps
// their icon, some text and their chevron; the number, length and remove
// stay fixed.

fn number_column<E: Styled>(element: E) -> E {
    element.w(px(NUMBER_WIDTH)).flex_none()
}

fn song_column<E: Styled>(element: E) -> E {
    element
        .flex_grow()
        .flex_shrink_0()
        .flex_basis(px(0.))
        .min_w(px(150.))
}

fn arrangement_column<E: Styled>(element: E) -> E {
    element.flex_shrink().flex_basis(px(150.)).min_w(px(96.))
}

fn transition_column<E: Styled>(element: E) -> E {
    element.flex_shrink().flex_basis(px(150.)).min_w(px(108.))
}

fn length_column<E: Styled>(element: E) -> E {
    element.w(px(48.)).flex_none()
}

fn remove_column<E: Styled>(element: E) -> E {
    element.w(px(32.)).flex_none()
}

fn editor_row(
    row: &EditorRow,
    rows: usize,
    insertion: Option<usize>,
    cx: &mut Context<Perform>,
) -> AnyElement {
    let index = row.index;
    let (badge, transition_icon) = (
        if row.click { "metronome" } else { "note" },
        transition_icon(row.transition),
    );
    // The whole row drags, the grip says so; the arrows stay for the
    // keyboard and as a fallback.
    let mut item = div()
        .id(("item-drag", index))
        .relative()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(10.))
        .py(px(8.))
        .rounded(px(10.))
        .bg(theme::raised())
        .cursor_grab()
        .on_drag(
            DraggedItem {
                index,
                label: row.title.clone().into(),
            },
            {
                let screen = cx.entity();
                move |dragged: &DraggedItem, _, _, cx: &mut App| {
                    start_drag(&screen, &dragged.label, cx)
                }
            },
        )
        .on_drag_move(
            cx.listener(move |screen, event: &DragMoveEvent<DraggedItem>, _, cx| {
                if let Some(half) = half_under(&event.bounds, event.event.position) {
                    let from = event.drag(cx).index;
                    screen.library_drag_over(Dragged::Item(from), index, half, cx);
                }
            }),
        )
        .when(insertion == Some(index), |item| {
            item.child(insertion_line().top(px(-6.)))
        })
        .when(index + 1 == rows && insertion == Some(rows), |item| {
            item.child(insertion_line().bottom(px(-6.)))
        });
    item = if row.selected {
        item.border_2().border_color(theme::text())
    } else {
        item.border_1().border_color(theme::border())
    };
    let step = |name: &'static str, enabled: bool, action, cx: &mut Context<Perform>| {
        let color = if enabled {
            theme::text_muted()
        } else {
            theme::border()
        };
        button(id(format!("{name}-{index}")), cx, act(action))
            .size(px(28.))
            .justify_center()
            .rounded(px(6.))
            .child(icon(name, color, 16.))
    };
    item.child(
        div()
            .min_h(px(48.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                number_column(div())
                    .flex()
                    .items_center()
                    .gap(px(NUMBER_GAP))
                    .child(icon("grip", theme::text_faint(), GRIP_SIZE))
                    .child(
                        div()
                            .w(px(ARROWS_WIDTH))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(step("up", index > 0, LibraryAction::MoveUp(index), cx))
                            .child(step(
                                "down",
                                index + 1 < rows,
                                LibraryAction::MoveDown(index),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .font_family(theme::MONO)
                            .text_size(px(13.))
                            .text_color(theme::text_muted())
                            .child(row.number.clone()),
                    ),
            )
            .child(
                song_column(button(
                    id(format!("row-{index}")),
                    cx,
                    act(LibraryAction::SelectRow(index)),
                ))
                .gap(px(10.))
                .child(
                    div()
                        .size(px(36.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(9.))
                        .bg(theme::badge())
                        .child(icon(badge, theme::text_soft(), 18.)),
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
                                .one_line()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(row.title.clone()),
                        )
                        .child(
                            div()
                                .one_line()
                                .text_size(px(13.))
                                .text_color(theme::text_muted())
                                .child(row.meta.clone()),
                        ),
                ),
            )
            .child(arrangement_column(row_dropdown(
                format!("row-arrangement-{index}"),
                "arrangement",
                row.arrangement.clone(),
                row,
                cx,
            )))
            .child(transition_column(row_dropdown(
                format!("row-transition-{index}"),
                transition_icon,
                transition_name(row.transition),
                row,
                cx,
            )))
            .child(
                length_column(div())
                    .text_right()
                    .font_family(theme::MONO)
                    .text_size(px(13.))
                    .child(row.length.clone()),
            )
            .child(
                remove_column(button(
                    id(format!("remove-{index}")),
                    cx,
                    act(LibraryAction::Remove(index)),
                ))
                .h(px(32.))
                .justify_center()
                .rounded(px(8.))
                .child(icon("close", theme::text_muted(), 16.)),
            ),
    )
    .children(row.problems.iter().map(|problem| {
        div()
            .pl(px(DETAILS_INDENT))
            .text_size(px(12.))
            .text_color(theme::danger())
            .child(problem.clone())
    }))
    .children(row.details.as_ref().map(|details| match details {
        RowDetails::Song(song) => song_details(index, song, row.transition, cx).into_any_element(),
        RowDetails::Click(click) => {
            click_details(index, click, row.transition, cx).into_any_element()
        }
    }))
    .into_any_element()
}

/// The arrangement or the transition of an item's row, drawn as the
/// design's dropdown: it opens the item's controls below the row, where
/// they are chosen, and closes them again.
fn row_dropdown(
    name: String,
    icon_name: &'static str,
    label: impl Into<SharedString>,
    row: &EditorRow,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let chevron = if row.selected { "up" } else { "down" };
    button(id(name), cx, act(LibraryAction::SelectRow(row.index)))
        .h(px(40.))
        .px(px(10.))
        .gap(px(6.))
        .rounded(px(8.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .child(icon(icon_name, theme::text_faint(), 15.))
        .child(div().flex_1().min_w_0().one_line().child(label.into()))
        .child(icon(chevron, theme::text(), 14.))
}

/// One labelled line of an item's controls.
fn control(label: &'static str) -> Div {
    div().flex().flex_wrap().items_center().gap(px(6.)).child(
        div()
            .w(px(96.))
            .flex_none()
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text_muted())
            .child(label),
    )
}

/// A choice among several; the chosen one is filled.
fn choice(
    name: String,
    label: impl Into<gpui::SharedString>,
    selected: bool,
    intent: Intent,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let (bg, fg) = if selected {
        (theme::selected(), theme::text())
    } else {
        (theme::surface(), theme::text_muted())
    };
    button(id(name), cx, intent)
        .h(px(36.))
        .px(px(10.))
        .rounded(px(8.))
        .bg(bg)
        .border_1()
        .border_color(theme::border())
        .text_color(fg)
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.into())
}

/// A value with a step down and a step up.
fn stepper(
    name: &'static str,
    index: usize,
    value: String,
    (down, can_down): (LibraryAction, bool),
    (up, can_up): (LibraryAction, bool),
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let side = |icon_name: &'static str, action, enabled: bool, cx: &mut Context<Perform>| {
        let color = if enabled {
            theme::text()
        } else {
            theme::border()
        };
        small_square(id(format!("{name}-{icon_name}-{index}")), cx, act(action))
            .child(icon(icon_name, color, 16.))
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(side("minus", down, can_down, cx))
        .child(
            div()
                .min_w(px(140.))
                .text_size(px(13.))
                .font_family(theme::MONO)
                .child(value),
        )
        .child(side("plus", up, can_up, cx))
}

fn transitions(index: usize, current: Transition, cx: &mut Context<Perform>) -> Div {
    control("Transition").children(TRANSITIONS.iter().map(|&transition| {
        choice(
            format!("transition-{index}-{}", transition_name(transition)),
            transition_name(transition),
            transition == current,
            act(LibraryAction::Transition(index, transition)),
            cx,
        )
        .child(icon(transition_icon(transition), theme::text_muted(), 16.))
    }))
}

fn song_details(
    index: usize,
    song: &SongDetails,
    transition: Transition,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pl(px(DETAILS_INDENT))
        .pb(px(4.))
        .child(
            control("Arrangement").children(song.arrangements.iter().enumerate().map(
                |(k, arrangement)| {
                    choice(
                        format!("arrangement-{index}-{k}"),
                        arrangement.label.clone(),
                        arrangement.selected,
                        act(LibraryAction::Arrangement(index, k)),
                        cx,
                    )
                },
            )),
        )
        .child(control("Start").child(stepper(
            "start",
            index,
            song.start.clone(),
            (LibraryAction::StartEarlier(index), song.start_earlier),
            (LibraryAction::StartLater(index), song.start_later),
            cx,
        )))
        .child(control("End").child(stepper(
            "end",
            index,
            song.end.clone(),
            (LibraryAction::EndEarlier(index), song.end_earlier),
            (LibraryAction::EndLater(index), song.end_later),
            cx,
        )))
        .child(control("Fade-out").child(choice(
            format!("fade-out-{index}"),
            if song.fade_out {
                "Fades out over its last 5 s"
            } else {
                "No fade-out"
            },
            song.fade_out,
            act(LibraryAction::ToggleFadeOut(index)),
            cx,
        )))
        .child(transitions(index, transition, cx))
}

fn click_details(
    index: usize,
    click: &ClickDetails,
    transition: Transition,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pl(px(DETAILS_INDENT))
        .pb(px(4.))
        .child(control("Tempo").child(stepper(
            "bpm",
            index,
            click.bpm.clone(),
            (LibraryAction::BpmDown(index), true),
            (LibraryAction::BpmUp(index), true),
            cx,
        )))
        .child(
            control("Meter").children(click.meters.iter().enumerate().map(|(k, meter)| {
                choice(
                    format!("meter-{index}-{k}"),
                    meter.label.clone(),
                    meter.selected,
                    act(LibraryAction::Meter(index, k)),
                    cx,
                )
            })),
        )
        .child(control("Length").child(stepper(
            "bars",
            index,
            click.bars.clone(),
            (LibraryAction::BarsDown(index), true),
            (LibraryAction::BarsUp(index), true),
            cx,
        )))
        .child(transitions(index, transition, cx))
}

/// The library's songs, with the search and a button that adds each one
/// to the setlist in the editor.
fn songs(view: &LibraryView, cx: &mut Context<Perform>) -> impl IntoElement {
    panel()
        .w(px(360.))
        .flex_none()
        .child(
            div()
                .flex()
                .items_center()
                .child(heading("note", "Songs"))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child(view.song_summary.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child("Search songs"),
                )
                .child(
                    button("search", cx, act(LibraryAction::Search))
                        .w_full()
                        .child(text_field(
                            &view.search,
                            view.searching,
                            "Title, key or tempo",
                            Some("search"),
                        )),
                ),
        )
        .children(view.unreadable_songs.iter().map(|line| {
            div()
                .text_size(px(12.))
                .text_color(theme::danger())
                .child(line.clone())
        }))
        .child(
            div()
                .id("songs")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(view.songs.iter().map(|song| song_row(song, cx))),
        )
}

fn song_row(song: &SongRow, cx: &mut Context<Perform>) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .min_h(px(56.))
        .border_b_1()
        .border_color(theme::divider())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .one_line()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(song.title.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .overflow_hidden()
                        .font_family(theme::MONO)
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(song_fact("note", &song.key))
                        .child(song_fact("metronome", &song.bpm))
                        .child(song_fact("clock", &song.length))
                        .child(song_fact("stems", &song.stems)),
                ),
        )
        .child(
            small_square(
                ("add-song", song.index),
                cx,
                act(LibraryAction::AddSong(song.index)),
            )
            .size(px(44.))
            .child(icon("plus", theme::text(), 16.)),
        )
        .into_any_element()
}

/// One fact of a song of the list, after its icon: its key, tempo, length
/// or stems.
fn song_fact(name: &'static str, text: &str) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.))
        .child(icon(name, theme::text_faint(), 12.))
        .child(text.to_owned())
}

/// A one-line field, after its `leading` icon: its text with a caret while
/// it has the focus, or the placeholder.
fn text_field(
    text: &str,
    focused: bool,
    placeholder: &'static str,
    leading: Option<&'static str>,
) -> impl IntoElement {
    let (shown, color) = match (text.is_empty(), focused) {
        (true, false) => (placeholder.to_owned(), theme::text_faint()),
        (_, true) => (format!("{text}▏"), theme::text()),
        (false, false) => (text.to_owned(), theme::text()),
    };
    div()
        .w_full()
        .h(px(44.))
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .rounded(px(9.))
        .bg(theme::bg())
        .border_1()
        .border_color(if focused {
            theme::accent()
        } else {
            theme::border()
        })
        .text_size(px(15.))
        .text_color(color)
        .when_some(leading, |field, name| {
            field.child(icon(name, theme::text_faint(), 18.))
        })
        .child(div().min_w_0().one_line().child(shown))
}

fn small_square(
    id: impl Into<ElementId>,
    cx: &mut Context<Perform>,
    intent: Intent,
) -> Stateful<Div> {
    button(id, cx, intent)
        .size(px(36.))
        .justify_center()
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
}

fn dashed_button(id: &'static str, cx: &mut Context<Perform>, intent: Intent) -> Stateful<Div> {
    button(id, cx, intent)
        .h(px(44.))
        .px(px(14.))
        .border_1()
        .border_dashed()
        .border_color(theme::chip_border())
        .text_size(px(14.))
        .text_color(theme::text_muted())
}

fn transition_icon(transition: Transition) -> &'static str {
    match transition {
        Transition::CueNext => "cue",
        Transition::Stay => "stay",
        Transition::AutoLink => "link",
        Transition::Crossfade => "crossfade",
    }
}
