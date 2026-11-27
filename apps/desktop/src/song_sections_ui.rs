//! The Song editor of a song of the library, drawn: a [`SongEditView`]
//! after the design canvas's Song editor artboard. The header, the notes
//! and the stems panel are the import's ([`crate::song_editor_ui`]); this
//! adds the "Section markers" panel (the bar ruler, the marker strip and
//! the waveform, with the cursor, the selected section and a dragged
//! marker), the marker inspector and the "Arrangement" panel. It reports
//! what the person does as an [`Intent`] or a drag, and decides nothing:
//! positions go to the view model as shares of the bars shown
//! ([`openvamp_desktop::song_sections::ALONG`]).
//!
//! Beside the canvas: a song's tempo and meter changes, when it has some,
//! show under the inspector; the arrangement shown is renamed and deleted
//! with the two buttons after "New"; the cursor is drawn in the queued
//! color.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Div, DragMoveEvent, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, Pixels, Rgba, ScrollWheelEvent, SharedString, Size, Stateful, canvas, div,
    prelude::*, px, relative,
};
use openvamp_desktop::action::Intent;
use openvamp_desktop::library_page::LibraryAction;
use openvamp_desktop::song_edit::SongAction;
use openvamp_desktop::song_import::FieldView;
use openvamp_desktop::song_sections::{
    ALONG, ArrangementView, ChipView, InspectorView, MarkersView, SongEditView, Span, WaveformState,
};

use crate::screen::Perform;
use crate::song_editor_ui::{
    DISABLED_OPACITY, GAP, HeaderStyle, PANEL_GAP, PANEL_PADDING, change_row, header, notes_lines,
    notes_panel, panel, panel_heading, stems_panel, stems_that_fit,
};
use crate::theme;
use crate::ui::{DragPreview, button, drag_preview, half_across, hint, icon, one_line_text};

/// Every icon this module draws by name (through [`icon`],
/// [`panel_heading`] and its summary figures), besides the sections' own
/// ([`openvamp_desktop::view::section_icons`]), for the app to carry each
/// one.
#[cfg(test)]
pub(crate) const ICONS: [&str; 18] = [
    "arrangement",
    "bar-lines",
    "check",
    "clock",
    "close",
    "down",
    "flag",
    "flag-plus",
    "grip",
    "infinite",
    "magnet",
    "metronome",
    "pencil",
    "plus",
    "speech",
    "trash",
    "zoom-in",
    "zoom-out",
];

/// The height of the "Section markers" panel, as on the canvas.
const MARKERS_HEIGHT: f32 = 340.;
/// The width of the inspector, as on the canvas.
const INSPECTOR_WIDTH: f32 = 330.;
/// The width of the stems panel, as on the canvas.
const STEMS_WIDTH: f32 = 320.;
/// The height of the bar ruler.
const RULER_HEIGHT: f32 = 14.;
/// The height of the marker strip.
const STRIP_HEIGHT: f32 = 44.;
/// The opacity of a waveform bar, as on the canvas.
const BAR_OPACITY: f32 = 0.8;
/// The opacity of a flat bar while the waveform is read.
const FLAT_OPACITY: f32 = 0.35;

/// A marker being dragged by the pointer.
pub(crate) struct DraggedMarker {
    index: usize,
    label: SharedString,
}

/// A chip of the arrangement being dragged by the pointer.
pub(crate) struct DraggedChip {
    index: usize,
    label: SharedString,
}

fn song(action: SongAction) -> Intent {
    Intent::Library(LibraryAction::Song(action))
}

/// How far across `bounds` the pointer at `x` is, out of [`ALONG`].
fn along(bounds: Bounds<Pixels>, x: Pixels) -> u32 {
    let width = f32::from(bounds.size.width).max(1.);
    let share = (f32::from(x - bounds.origin.x) / width).clamp(0., 1.);
    (share * ALONG as f32).round() as u32
}

/// The Song editor of a song of the library.
pub fn song_edit(
    view: &SongEditView,
    viewport: Size<Pixels>,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let editor = &view.editor;
    let notes = notes_lines(editor);
    let style = HeaderStyle {
        back_hint: "Go back to the Library",
        maps_locked: view.maps_read_only,
    };
    let fit = stems_that_fit(editor, viewport, notes, MARKERS_HEIGHT + GAP);
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .child(header(editor, style, cx))
        .when(notes > 0, |page| page.child(notes_panel(editor)))
        .child(markers_panel(&view.markers, cx))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(GAP))
                .child(
                    div()
                        .id("inspector-column")
                        .w(px(INSPECTOR_WIDTH))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(GAP))
                        .overflow_y_scroll()
                        .child(inspector(&view.inspector, cx))
                        .when(!editor.changes.is_empty(), |column| {
                            column.child(changes(view, cx))
                        }),
                )
                .child(arrangement_panel(&view.arrangement, cx))
                .child(stems_panel(editor, fit, Some(STEMS_WIDTH), cx)),
        )
}

/// A 36-point square button of the markers panel.
fn square(
    id: &'static str,
    intent: Intent,
    enabled: bool,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    button(id, cx, intent)
        .size(px(36.))
        .justify_center()
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .when(!enabled, |button| button.opacity(DISABLED_OPACITY))
}

/// The "Section markers" panel: its controls, then the ruler, the strip
/// and the waveform, which share the bars shown.
fn markers_panel(view: &MarkersView, cx: &mut Context<Perform>) -> impl IntoElement {
    let snap = view.snap_to_bar;
    let snap_color = if snap { theme::accent() } else { theme::text() };
    div()
        .h(px(MARKERS_HEIGHT))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(PANEL_GAP))
        .px(px(14.))
        .py(px(12.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(panel_heading("flag", "Section markers"))
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child(one_line_text(
                            "Tap a section to edit it · drag a marker to move it",
                        )),
                )
                .child(div().flex_1())
                .child(
                    button("add-marker", cx, song(SongAction::AddMarker))
                        .h(px(36.))
                        .pl(px(10.))
                        .pr(px(12.))
                        .gap(px(6.))
                        .rounded(px(8.))
                        .bg(theme::text())
                        .text_color(theme::on_fill())
                        .text_size(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .tooltip(hint(format!("Add a marker at {}", view.cursor_label)))
                        .child(icon("flag-plus", theme::on_fill(), 16.))
                        .child("Add marker at playhead"),
                )
                .child(
                    button("snap", cx, song(SongAction::ToggleSnap))
                        .h(px(36.))
                        .pl(px(10.))
                        .pr(px(12.))
                        .gap(px(6.))
                        .rounded(px(8.))
                        .bg(theme::raised())
                        .border_1()
                        .border_color(if snap {
                            theme::accent()
                        } else {
                            theme::border()
                        })
                        .text_color(snap_color)
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .tooltip(hint(if snap {
                            "Markers snap to the nearest bar; off, to the nearest beat"
                        } else {
                            "Markers snap to the nearest beat; on, to the nearest bar"
                        }))
                        .child(icon("magnet", snap_color, 16.))
                        .child("Snap to bar"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(2.))
                        .child(
                            square("zoom-out", song(SongAction::ZoomOut), view.can_zoom_out, cx)
                                .tooltip(hint("Zoom out"))
                                .child(icon("zoom-out", theme::text(), 18.)),
                        )
                        .child(
                            square("zoom-in", song(SongAction::ZoomIn), view.can_zoom_in, cx)
                                .tooltip(hint("Zoom in"))
                                .child(icon("zoom-in", theme::text(), 18.)),
                        ),
                ),
        )
        .child(lane(view, cx))
}

/// The ruler, the strip and the waveform. A click on the ruler or the
/// waveform places the cursor; a marker dragged over any of them shows
/// where it lands; the wheel scrolls through the song.
fn lane(view: &MarkersView, cx: &mut Context<Perform>) -> impl IntoElement {
    // The lane's bounds, as last laid out, for the clicks.
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
    let record = Rc::clone(&bounds);
    let visible = view.visible_bars;
    let (on_ruler, on_waveform, wheel) =
        (Rc::clone(&bounds), Rc::clone(&bounds), Rc::clone(&bounds));
    div()
        .id("marker-lane")
        .relative()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(PANEL_GAP))
        .child(
            canvas(
                move |laid_out, _, _| record.set(Some(laid_out)),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .on_drag_move(
            cx.listener(move |screen, event: &DragMoveEvent<DraggedMarker>, _, cx| {
                let index = event.drag(cx).index;
                if event.bounds.contains(&event.event.position) {
                    let at = along(event.bounds, event.event.position.x);
                    screen.marker_drag_over(index, at, cx);
                } else {
                    screen.marker_drag_left(cx);
                }
            }),
        )
        .on_drop(cx.listener(|screen, marker: &DraggedMarker, _, cx| {
            screen.marker_drop(marker.index, cx);
        }))
        .on_scroll_wheel(
            cx.listener(move |screen, event: &ScrollWheelEvent, window, cx| {
                let Some(bounds) = wheel.get() else {
                    return;
                };
                let delta = event.delta.pixel_delta(px(16.));
                let pixels = -f32::from(delta.x + delta.y);
                let bars = pixels * visible as f32 / f32::from(bounds.size.width).max(1.);
                let thousandths = (bars * 1000.).round() as i32;
                if thousandths != 0 {
                    screen.handle(song(SongAction::Scroll(thousandths)), window, cx);
                }
            }),
        )
        .child(ruler(view).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |screen, event: &MouseDownEvent, window, cx| {
                place_cursor(screen, on_ruler.get(), event, window, cx);
            }),
        ))
        .child(strip(view, cx))
        .child(waveform(view).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |screen, event: &MouseDownEvent, window, cx| {
                place_cursor(screen, on_waveform.get(), event, window, cx);
            }),
        ))
}

/// A click at `event` over the lane laid out in `bounds`: the cursor goes
/// there.
fn place_cursor(
    screen: &mut Perform,
    bounds: Option<Bounds<Pixels>>,
    event: &MouseDownEvent,
    window: &mut gpui::Window,
    cx: &mut Context<Perform>,
) {
    if let Some(bounds) = bounds {
        let at = along(bounds, event.position.x);
        screen.handle(song(SongAction::PlaceCursor(at)), window, cx);
    }
}

/// A vertical line across its parent at `left` (a share of its width).
fn line(left: f32, width: f32, color: Rgba) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(relative(left.clamp(0., 1.)))
        .w(px(width))
        .bg(color)
}

/// The bar ruler: a label every 8 bars, and the cursor.
fn ruler(view: &MarkersView) -> Div {
    div()
        .relative()
        .h(px(RULER_HEIGHT))
        .flex_none()
        .cursor_pointer()
        .children(view.ruler.iter().map(|mark| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(mark.left))
                .border_l_1()
                .border_color(theme::chip_border())
                .pl(px(4.))
                .font_family(theme::MONO)
                .text_size(px(11.))
                .line_height(px(RULER_HEIGHT))
                .text_color(theme::text_faint())
                .child(mark.bar.to_string())
        }))
        .when_some(view.cursor, |ruler, cursor| {
            ruler.child(line(cursor, 2., theme::queued()))
        })
}

/// The marker strip: one block per section, as wide as its bars.
fn strip(view: &MarkersView, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .relative()
        .h(px(STRIP_HEIGHT))
        .flex_none()
        .children(view.blocks.iter().map(|block| {
            let color = theme::section(block.kind);
            let label: SharedString = block.label.clone().into();
            let tip = format!("{} · starts at {}", block.label, block.starts);
            let body = button(
                ("marker", block.index),
                cx,
                song(SongAction::SelectSection(block.index)),
            )
            .size_full()
            .flex_col()
            .items_start()
            .justify_center()
            .gap(px(1.))
            .px(px(6.))
            .rounded(px(6.))
            .bg(color)
            .overflow_hidden()
            .text_color(theme::on_fill())
            .when(block.selected, |body| {
                body.border_2().border_color(theme::text())
            })
            .tooltip(hint(if block.movable {
                format!("{tip} · drag to move it")
            } else {
                tip
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .font_family(theme::CONDENSED)
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(14.))
                    .line_height(px(15.))
                    .child(icon(block.icon, theme::on_fill(), 12.))
                    .child(one_line_text(block.abbr.clone())),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .line_height(px(12.))
                    .child(one_line_text(block.starts.clone())),
            );
            let body = if block.movable {
                body.cursor_grab().on_drag(
                    DraggedMarker {
                        index: block.index,
                        label,
                    },
                    |dragged: &DraggedMarker, _, _, cx: &mut App| -> gpui::Entity<DragPreview> {
                        drag_preview(&dragged.label, cx)
                    },
                )
            } else {
                body
            };
            spanned(block.span).child(body).into_any_element()
        }))
        .when_some(view.drag.as_ref(), |strip, ghost| {
            strip.child(line(ghost.left, 2., theme::text()))
        })
}

/// A box over `span` of its parent, a point in from each side, so blocks
/// side by side keep a gap.
fn spanned(span: Span) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(relative(span.left))
        .w(relative(span.width))
        .px(px(1.))
}

/// The waveform: one bar per bar shown in its section's color, the
/// selected section, the cursor, a dragged marker's landing place, and a
/// handle on each marker line to drag it by.
fn waveform(view: &MarkersView) -> Div {
    let flat = view.waveform != WaveformState::Ready;
    let opacity = if flat { FLAT_OPACITY } else { BAR_OPACITY };
    let status = match &view.waveform {
        WaveformState::Ready => None,
        WaveformState::Reading { percent } => Some((
            format!("Reading the stems for the waveform… {percent}%"),
            theme::text_muted(),
        )),
        WaveformState::Failed(problem) => Some((problem.clone(), theme::danger_text())),
    };
    // The canvas's 2-point gap between bars, narrower when many bars show,
    // so the gaps never take the bars' room.
    let gap = match view.bars.len() {
        0..=160 => 2.,
        161..=320 => 1.,
        _ => 0.,
    };
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .gap(px(gap))
        .px(px(2.))
        .rounded(px(8.))
        .bg(theme::panel())
        .overflow_hidden()
        .cursor_pointer()
        .when_some(view.selection, |wave, span| {
            wave.child(
                spanned(span)
                    .px_0()
                    .bg(theme::faded(theme::text(), 0.07))
                    .child(
                        div()
                            .size_full()
                            .relative()
                            .child(line(0., 2., theme::text()))
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .right_0()
                                    .w(px(1.))
                                    .bg(theme::selection_edge()),
                            ),
                    ),
            )
        })
        .children(view.bars.iter().map(|bar| {
            div()
                .flex_1()
                .min_w_0()
                .h(relative(bar.height))
                .rounded(px(1.))
                .bg(theme::faded(theme::section(bar.kind), opacity))
        }))
        .children(view.blocks.iter().filter_map(|block| {
            // Only a marker that starts inside the bars shown has a line.
            if !block.movable || block.span.left <= 0. {
                return None;
            }
            let handle = div()
                .id(("marker-handle", block.index))
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(block.span.left))
                .ml(px(-5.))
                .w(px(10.))
                .cursor_ew_resize()
                .on_drag(
                    DraggedMarker {
                        index: block.index,
                        label: block.label.clone().into(),
                    },
                    |dragged: &DraggedMarker, _, _, cx: &mut App| -> gpui::Entity<DragPreview> {
                        drag_preview(&dragged.label, cx)
                    },
                );
            Some(handle.into_any_element())
        }))
        .when_some(view.cursor, |wave, cursor| {
            wave.child(line(cursor, 2., theme::queued()))
        })
        .when_some(view.drag.as_ref(), |wave, ghost| {
            wave.child(line(ghost.left, 2., theme::text())).child(
                div()
                    .absolute()
                    .top(px(6.))
                    .left(relative(ghost.left))
                    .ml(px(6.))
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(5.))
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .font_family(theme::MONO)
                    .text_size(px(11.))
                    .child(ghost.label.clone()),
            )
        })
        .when_some(status, |wave, (text, color)| {
            wave.child(
                div()
                    .absolute()
                    .bottom(px(8.))
                    .left(px(10.))
                    .text_size(px(12.))
                    .text_color(color)
                    .child(text),
            )
        })
}

/// A label above a value, as the inspector lays out its facts.
fn fact(label: &'static str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .min_w_0()
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(label),
        )
        .child(value)
}

/// A boxed value of the inspector, as the canvas draws Type and Number.
fn boxed(
    id: &'static str,
    intent: Intent,
    editing: bool,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    button(id, cx, intent)
        .h(px(36.))
        .px(px(10.))
        .gap(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(if editing {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(theme::bg())
        .text_size(px(15.))
}

/// The marker inspector: the selected section's type, number, start and
/// length, its loop switch for the arrangement shown, and "Delete marker".
fn inspector(view: &InspectorView, cx: &mut Context<Perform>) -> impl IntoElement {
    let color = theme::section(view.kind);
    let number = shown_field(&view.number, "—");
    panel()
        .flex_1()
        .min_h(px(300.))
        .gap(px(14.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_none()
                        .size(px(36.))
                        .rounded(px(10.))
                        .bg(color)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(view.icon, theme::on_fill(), 20.)),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .child(one_line_text(view.name.clone())),
                ),
        )
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap(px(10.))
                .child(fact(
                    "Type",
                    boxed(
                        "section-type",
                        song(SongAction::ToggleTypeMenu),
                        view.type_menu.is_some(),
                        cx,
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(one_line_text(view.section_type.clone())),
                    )
                    .child(icon("down", theme::text_muted(), 14.)),
                ))
                .child(fact(
                    "Number",
                    boxed(
                        "section-number",
                        song(SongAction::EditNumber),
                        view.number.editing,
                        cx,
                    )
                    .cursor_text()
                    .font_family(theme::MONO)
                    .text_color(number.1)
                    .child(div().min_w_0().child(one_line_text(number.0))),
                ))
                .child(fact(
                    "Starts at",
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(15.))
                        .child(view.starts_at.clone()),
                ))
                .child(fact(
                    "Length",
                    div()
                        .font_family(theme::MONO)
                        .text_size(px(15.))
                        .child(view.length.clone()),
                )),
        )
        .when_some(view.type_menu.as_ref(), |inspector, menu| {
            inspector.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .p(px(8.))
                    .rounded(px(10.))
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .children(menu.iter().enumerate().map(|(index, choice)| {
                        let (bg, fg) = if choice.selected {
                            (theme::selected(), theme::text())
                        } else {
                            (theme::panel(), theme::text_muted())
                        };
                        button(
                            ("section-type-choice", index),
                            cx,
                            song(SongAction::SetType(index)),
                        )
                        .h(px(28.))
                        .px(px(8.))
                        .gap(px(4.))
                        .rounded(px(6.))
                        .bg(bg)
                        .text_size(px(13.))
                        .text_color(fg)
                        .child(choice.label.clone())
                        .when(choice.selected, |item| {
                            item.child(icon("check", theme::accent(), 12.))
                        })
                    })),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .min_h(px(48.))
                .pt(px(10.))
                .border_t_1()
                .border_color(theme::border())
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
                                .gap(px(8.))
                                .text_size(px(15.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(icon("infinite", theme::text_muted(), 16.))
                                .child("Infinite loop by default"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme::text_muted())
                                .child(view.loop_hint.clone()),
                        ),
                )
                .child(switch(view.infinite_loop, cx)),
        )
        .child(div().flex_1())
        .child(
            button("delete-marker", cx, song(SongAction::DeleteMarker))
                .h(px(44.))
                .justify_center()
                .rounded(px(10.))
                .border_1()
                .border_color(theme::danger_border())
                .text_color(theme::danger_hint())
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .when_some(view.delete_hint.clone(), |button, why| {
                    button.opacity(DISABLED_OPACITY).tooltip(hint(why))
                })
                .child(icon("trash", theme::danger_hint(), 16.))
                .child("Delete marker"),
        )
}

/// What a field shows: its text (with a caret while typed into) or
/// `placeholder`, and the color to draw it in.
fn shown_field(field: &FieldView, placeholder: &str) -> (String, Rgba) {
    match (field.text.is_empty(), field.editing) {
        (true, false) => (placeholder.to_owned(), theme::text_faint()),
        (_, true) => (format!("{}▏", field.text), theme::text()),
        (false, false) => (field.text.clone(), theme::text()),
    }
}

/// The loop switch: a 48 × 28 track with its thumb.
fn switch(on: bool, cx: &mut Context<Perform>) -> impl IntoElement {
    button("infinite-loop", cx, song(SongAction::ToggleLoop))
        .w(px(48.))
        .h(px(28.))
        .p(px(3.))
        .rounded(px(14.))
        .bg(if on {
            theme::accent()
        } else {
            theme::chip_border()
        })
        .when(on, |track| track.justify_end())
        .child(div().size(px(22.)).rounded(px(11.)).bg(theme::text()))
}

/// The song's tempo and meter changes, under the inspector.
fn changes(view: &SongEditView, cx: &mut Context<Perform>) -> impl IntoElement {
    let editor = &view.editor;
    panel()
        .flex_none()
        .gap(px(PANEL_GAP))
        .child(panel_heading("metronome", "Tempo and meter"))
        .child(
            div()
                .text_size(px(13.))
                .font_family(theme::MONO)
                .text_color(theme::text_muted())
                .child(one_line_text(format!(
                    "Bar 1 · {} BPM · {}",
                    editor.tempo.text, editor.time.text
                ))),
        )
        .children(editor.changes.iter().map(|change| change_row(change, cx)))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::text_faint())
                .child("Leave Tempo or Time empty to keep it."),
        )
}

/// The "Arrangement" panel: the tabs, the chips of the arrangement shown,
/// the sections to add at its end, and its summary.
fn arrangement_panel(view: &ArrangementView, cx: &mut Context<Perform>) -> impl IntoElement {
    let editable = view.editable;
    let chips = view.chips.len();
    panel()
        .flex_1()
        .min_w_0()
        .gap(px(PANEL_PADDING))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .mr(px(8.))
                        .child(panel_heading("arrangement", "Arrangement")),
                )
                .children(view.tabs.iter().map(|tab| {
                    let tab_button = button(
                        ("arrangement-tab", tab.index),
                        cx,
                        song(SongAction::SelectArrangement(tab.index)),
                    )
                    .h(px(32.))
                    .px(px(12.))
                    .rounded(px(8.))
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD);
                    match &tab.renaming {
                        Some(field) => {
                            let (text, color) = shown_field(field, "");
                            button(
                                ("arrangement-name", tab.index),
                                cx,
                                song(SongAction::RenameArrangement),
                            )
                            .h(px(32.))
                            .min_w(px(120.))
                            .px(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(theme::accent())
                            .bg(theme::bg())
                            .cursor_text()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(text)
                            .into_any_element()
                        }
                        None if tab.active => tab_button
                            .bg(theme::text())
                            .text_color(theme::on_fill())
                            .child(tab.name.clone())
                            .into_any_element(),
                        None => tab_button
                            .bg(theme::raised())
                            .border_1()
                            .border_color(theme::border())
                            .child(tab.name.clone())
                            .into_any_element(),
                    }
                }))
                .child(
                    button("new-arrangement", cx, song(SongAction::NewArrangement))
                        .h(px(32.))
                        .pl(px(10.))
                        .pr(px(12.))
                        .gap(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_dashed()
                        .border_color(theme::chip_border())
                        .text_size(px(14.))
                        .text_color(theme::text_muted())
                        .child(icon("plus", theme::text_muted(), 14.))
                        .child("New"),
                )
                .child(
                    small(
                        "rename-arrangement",
                        song(SongAction::RenameArrangement),
                        editable,
                        cx,
                    )
                    .tooltip(hint(if editable {
                        "Rename this arrangement"
                    } else {
                        "The Default arrangement keeps its name"
                    }))
                    .child(icon("pencil", theme::text_muted(), 14.)),
                )
                .child(
                    small(
                        "delete-arrangement",
                        song(SongAction::DeleteArrangement),
                        editable,
                        cx,
                    )
                    .tooltip(hint(if editable {
                        "Delete this arrangement"
                    } else {
                        "The Default arrangement cannot be deleted"
                    }))
                    .child(icon("trash", theme::text_muted(), 14.)),
                ),
        )
        .child(
            div()
                .id("arrangement-chips")
                .flex()
                .flex_wrap()
                .content_start()
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(10.))
                .bg(theme::bg())
                .border_1()
                .border_color(theme::divider())
                .min_h(px(150.))
                .on_drag_move(
                    cx.listener(|screen, event: &DragMoveEvent<DraggedChip>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            screen.chip_drag_left(cx);
                        }
                    }),
                )
                .on_drop(cx.listener(|screen, chip: &DraggedChip, _, cx| {
                    screen.chip_drop(chip.index, cx);
                }))
                .children(
                    view.chips
                        .iter()
                        .map(|chip| chip_view(chip, view.insertion, chips, cx)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child("Add a section to the end · drag chips to reorder"),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .children(view.palette.iter().map(|section| {
                            button(
                                ("append-section", section.index),
                                cx,
                                song(SongAction::AppendSection(section.index)),
                            )
                            .h(px(32.))
                            .px(px(10.))
                            .gap(px(6.))
                            .rounded(px(8.))
                            .bg(theme::raised())
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(13.))
                            .child(icon(section.icon, theme::section(section.kind), 14.))
                            .child(section.label.clone())
                        })),
                ),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .pt(px(10.))
                .border_t_1()
                .border_color(theme::border())
                .text_size(px(14.))
                .text_color(theme::text_muted())
                .child(total(
                    "arrangement",
                    &view.sections,
                    plural_word(&view.sections, "section"),
                ))
                .child(total(
                    "bar-lines",
                    &view.bars,
                    plural_word(&view.bars, "bar"),
                ))
                .child(total("clock", &view.time, String::new()))
                .child(div().flex_1())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .child(icon("speech", theme::text_muted(), 15.))
                        .child(one_line_text("Guide cues follow this order automatically")),
                ),
        )
}

/// " sections" after a count of `count`, or " section" after 1.
fn plural_word(count: &str, word: &str) -> String {
    if count == "1" {
        format!(" {word}")
    } else {
        format!(" {word}s")
    }
}

/// One figure of the summary: its icon, the figure, and what it counts.
fn total(name: &'static str, figure: &str, unit: String) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(icon(name, theme::text_muted(), 15.))
        .child(
            div()
                .font_family(theme::MONO)
                .text_color(theme::text())
                .child(figure.to_owned()),
        )
        .when(!unit.is_empty(), |total| total.child(unit))
}

/// A 32-point square button of the arrangement panel's header.
fn small(
    id: &'static str,
    intent: Intent,
    enabled: bool,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    button(id, cx, intent)
        .size(px(32.))
        .justify_center()
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .when(!enabled, |button| button.opacity(DISABLED_OPACITY))
}

/// A chip: its grip, its section's icon, abbreviation and bars, and its ×;
/// dragged by the grip or anywhere on it.
fn chip_view(
    chip: &ChipView,
    insertion: Option<usize>,
    chips: usize,
    cx: &mut Context<Perform>,
) -> AnyElement {
    let index = chip.index;
    let insertion_line = || {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(3.))
            .rounded(px(2.))
            .bg(theme::accent())
    };
    div()
        .id(("chip", index))
        .relative()
        .h(px(40.))
        .pl(px(4.))
        .pr(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .cursor_grab()
        .tooltip(hint(format!("{} · drag to reorder", chip.label)))
        .on_drag(
            DraggedChip {
                index,
                label: chip.label.clone().into(),
            },
            |dragged: &DraggedChip, _, _, cx: &mut App| drag_preview(&dragged.label, cx),
        )
        .on_drag_move(
            cx.listener(move |screen, event: &DragMoveEvent<DraggedChip>, _, cx| {
                if let Some(half) = half_across(&event.bounds, event.event.position) {
                    let from = event.drag(cx).index;
                    screen.chip_drag_over(from, index, half, cx);
                }
            }),
        )
        .when(insertion == Some(index), |chip| {
            chip.child(insertion_line().left(px(-6.)))
        })
        .when(index + 1 == chips && insertion == Some(chips), |chip| {
            chip.child(insertion_line().right(px(-6.)))
        })
        .child(icon("grip", theme::text_faint(), 14.))
        .child(icon(chip.icon, theme::section(chip.kind), 16.))
        .child(
            div()
                .font_family(theme::CONDENSED)
                .font_weight(FontWeight::BOLD)
                .text_size(px(16.))
                .child(chip.abbr.clone()),
        )
        .when(chip.looped, |line| {
            line.child(icon("infinite", theme::accent(), 12.))
        })
        .child(
            div()
                .font_family(theme::MONO)
                .text_size(px(11.))
                .text_color(theme::text_faint())
                .child(chip.bars.clone()),
        )
        .child(
            button(
                ("remove-chip", index),
                cx,
                song(SongAction::RemoveChip(index)),
            )
            .size(px(24.))
            .justify_center()
            .rounded(px(6.))
            .tooltip(hint(format!("Remove {} from the arrangement", chip.label)))
            .child(icon("close", theme::text_faint(), 14.)),
        )
        .into_any_element()
}
