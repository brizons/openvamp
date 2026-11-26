//! The Song editor's presentational code: draws a [`SongEditorView`] after
//! the design canvas's Song editor artboard (the header with the song's
//! facts, Discard and Save song, and the stems panel) and reports what the
//! person does as an [`Intent`]. It decides nothing.
//!
//! The canvas's section markers and arrangement panels are F2 and are not
//! drawn: the tempo and meter changes take the left of the screen, and the
//! stems the rest. A stem's part menu opens under its row, as the setlist
//! editor's item controls and the Mixer's bus choices do.

use domain::Bus;
use gpui::{
    AlignSelf, AnyElement, Context, Div, FontWeight, IntoElement, Pixels, Size, Stateful, div,
    prelude::*, px, relative,
};
use openvamp_desktop::action::Intent;
use openvamp_desktop::library_page::LibraryAction;
use openvamp_desktop::song_import::{
    ChangeRow, FieldView, ImportAction, PartGroup, PartMenu, SongEditorView, SongField, StemRow,
};

use crate::screen::Perform;
use crate::theme;
use crate::ui::{button, hint, icon, one_line_text};

/// How a control that does nothing now is drawn, as on the canvas
/// (`button:disabled`).
const DISABLED_OPACITY: f32 = 0.45;

/// The height of the header, as on the canvas.
const HEADER_HEIGHT: f32 = 72.;
/// The padding of [`crate::ui::main`], above and below the screen.
const SCREEN_PADDING: f32 = 16.;
/// The gap between the header, the notes and the panels.
const GAP: f32 = 12.;
/// The padding of a panel.
const PANEL_PADDING: f32 = 14.;
/// The gap between the rows of a panel's column.
const PANEL_GAP: f32 = 10.;
/// A stem row's height: 36 points and its divider.
const STEM_ROW: f32 = 37.;
/// The height of "Show N more".
const SHOW_MORE: f32 = 36.;
/// The tallest the notes above the panels get before they scroll.
const NOTES_MAX: f32 = 132.;
/// A line of the notes.
const NOTE_LINE: f32 = 19.;
/// The width of the tempo and meter panel: the canvas's inspector's.
const CHANGES_WIDTH: f32 = 330.;
/// The width of a part menu's column.
const MENU_COLUMN: f32 = 160.;

fn act(action: ImportAction) -> Intent {
    Intent::Library(LibraryAction::Import(action))
}

/// The Song editor.
pub fn song_editor(
    view: &SongEditorView,
    viewport: Size<Pixels>,
    cx: &mut Context<Perform>,
) -> impl IntoElement {
    let notes = notes_lines(view);
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .child(header(view, cx))
        .when(notes > 0, |page| page.child(notes_panel(view)))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(GAP))
                .child(changes_panel(view, cx))
                .child(stems_panel(view, stems_that_fit(view, viewport, notes), cx)),
        )
}

fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(PANEL_GAP))
        .p(px(PANEL_PADDING))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.))
}

fn panel_heading(name: &'static str, title: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(17.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon(name, theme::text_muted(), 18.))
        .child(title)
}

/// The header: back to the Library, the song's facts, "Tempo or meter
/// change", Discard and Save song.
fn header(view: &SongEditorView, cx: &mut Context<Perform>) -> impl IntoElement {
    let locked = view.saving.is_some();
    div()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .rounded(px(14.))
        .child(
            button("song-editor-back", cx, act(ImportAction::Discard))
                .gap(px(4.))
                .text_size(px(14.))
                .text_color(theme::text_muted())
                .tooltip(hint("Discard the import and go back to the Library"))
                .child(icon("left", theme::text_muted(), 16.))
                .child("Library"),
        )
        .child(labelled(
            "Title",
            None,
            field("song-title", &view.title, SongField::Title, TITLE, "", cx),
        ))
        .child(labelled(
            "Key",
            Some("note"),
            field("song-key", &view.key, SongField::Key, SHORT, "—", cx),
        ))
        .child(labelled(
            "Tempo",
            Some("metronome"),
            field("song-tempo", &view.tempo, SongField::Tempo, NUMBER, "", cx),
        ))
        .child(labelled(
            "Time",
            None,
            field("song-time", &view.time, SongField::Time, SHORT, "", cx),
        ))
        .child(
            to_the_bottom(button("add-change", cx, act(ImportAction::AddChange)))
                .mb(px(8.))
                .h(px(32.))
                .px(px(10.))
                .gap(px(6.))
                .rounded(px(7.))
                .border_1()
                .border_dashed()
                .border_color(theme::chip_border())
                .text_size(px(13.))
                .text_color(theme::text_muted())
                .when(locked, |button: Stateful<Div>| {
                    button.opacity(DISABLED_OPACITY)
                })
                .child(icon("plus", theme::text_muted(), 14.))
                .child("Tempo or meter change"),
        )
        .child(div().flex_1())
        .child(
            button("discard-song", cx, act(ImportAction::Discard))
                .h(px(44.))
                .px(px(14.))
                .rounded(px(10.))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::border())
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .when(
                    view.saving.as_ref().is_some_and(|saving| saving.cancelling),
                    |button| button.opacity(DISABLED_OPACITY),
                )
                .child(icon("close", theme::text(), 16.))
                .child("Discard"),
        )
        .child(save_button(view, cx))
}

/// `element`, aligned to the bottom of its row, as the canvas's
/// `align-self: flex-end`.
fn to_the_bottom<E: Styled>(mut element: E) -> E {
    element.style().align_self = Some(AlignSelf::FlexEnd);
    element
}

/// Save song: off while something blocks the song; while it saves, its
/// progress fills it.
fn save_button(view: &SongEditorView, cx: &mut Context<Perform>) -> impl IntoElement {
    let label = view
        .saving
        .as_ref()
        .map_or_else(|| "Save song".to_owned(), |saving| saving.label.clone());
    button("save-song", cx, act(ImportAction::Save))
        .relative()
        .overflow_hidden()
        .h(px(44.))
        .px(px(16.))
        .rounded(px(10.))
        .bg(theme::text())
        .text_color(theme::on_fill())
        .text_size(px(14.))
        .font_weight(FontWeight::BOLD)
        .when(!view.can_save && view.saving.is_none(), |button| {
            button
                .opacity(DISABLED_OPACITY)
                .tooltip(hint("Fix what the editor lists first"))
        })
        .when_some(view.saving.as_ref(), |button, saving| {
            // Drawn first, under the label.
            button.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative(saving.percent as f32 / 100.))
                    .bg(theme::faded(theme::accent(), 0.55)),
            )
        })
        .child(icon("check", theme::on_fill(), 16.))
        .child(label)
}

/// How a field is drawn: the canvas's sizes for each.
#[derive(Clone, Copy)]
struct FieldStyle {
    width: f32,
    mono: bool,
    size: f32,
    weight: FontWeight,
}

/// The title: 220 points of the interface font.
const TITLE: FieldStyle = FieldStyle {
    width: 220.,
    mono: false,
    size: 16.,
    weight: FontWeight::SEMIBOLD,
};

/// A key, a time signature or a bar: 56 points of monospace.
const SHORT: FieldStyle = FieldStyle {
    width: 56.,
    mono: true,
    size: 14.,
    weight: FontWeight::NORMAL,
};

/// A tempo: 64 points of monospace.
const NUMBER: FieldStyle = FieldStyle {
    width: 64.,
    mono: true,
    size: 14.,
    weight: FontWeight::NORMAL,
};

/// A field with its label above it, the label after `label_icon`.
fn labelled(
    label: &'static str,
    label_icon: Option<&'static str>,
    field: impl IntoElement,
) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .when_some(label_icon, |line, name| {
                    line.child(icon(name, theme::text_muted(), 12.))
                })
                .child(label),
        )
        .child(field)
}

/// A one-line field: its text, with a caret while the keys type into it,
/// or `placeholder` while it is empty. A click starts typing into it.
fn field(
    id: impl Into<String>,
    view: &FieldView,
    field: SongField,
    style: FieldStyle,
    placeholder: &'static str,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let (shown, color) = match (view.text.is_empty(), view.editing) {
        (true, false) => (placeholder.to_owned(), theme::text_faint()),
        (_, true) => (format!("{}▏", view.text), theme::text()),
        (false, false) => (view.text.clone(), theme::text()),
    };
    button(
        gpui::ElementId::Name(id.into().into()),
        cx,
        act(ImportAction::Edit(field)),
    )
    .w(px(style.width))
    .h(px(30.))
    .px(px(8.))
    .rounded(px(7.))
    .border_1()
    .border_color(if view.editing {
        theme::accent()
    } else {
        theme::border()
    })
    .bg(theme::bg())
    .cursor_text()
    .when(style.mono, |field| field.font_family(theme::MONO))
    .text_size(px(style.size))
    .font_weight(style.weight)
    .text_color(color)
    .child(div().min_w_0().child(one_line_text(shown)))
}

/// How many lines the notes above the panels hold.
fn notes_lines(view: &SongEditorView) -> usize {
    view.problems.len() + view.warnings.len() + usize::from(view.save_problem.is_some())
}

/// What keeps the song from being saved, in red, and the warnings that do
/// not, in amber: the `domain` and `library` crates' words.
fn notes_panel(view: &SongEditorView) -> impl IntoElement {
    let line = |text: &String, color| {
        div()
            .flex()
            .items_start()
            .gap(px(8.))
            .text_size(px(13.))
            .text_color(color)
            .child(div().flex_1().min_w_0().child(text.clone()))
    };
    let blocking = !view.problems.is_empty() || view.save_problem.is_some();
    let (bg, border) = if blocking {
        (theme::danger_bg(), theme::danger())
    } else {
        (theme::queued_bg(), theme::queued_border())
    };
    div()
        .id("song-editor-notes")
        .flex_none()
        .max_h(px(NOTES_MAX))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(14.))
        .py(px(10.))
        .bg(bg)
        .border_1()
        .border_color(border)
        .rounded(px(14.))
        .children(
            view.save_problem
                .iter()
                .chain(&view.problems)
                .map(|text| line(text, theme::danger_text())),
        )
        .children(view.warnings.iter().map(|text| line(text, theme::queued())))
}

/// The song's tempo and meter at the start, and the changes after it.
fn changes_panel(view: &SongEditorView, cx: &mut Context<Perform>) -> impl IntoElement {
    let start = format!("Bar 1 · {} BPM · {}", view.tempo.text, view.time.text);
    panel()
        .w(px(CHANGES_WIDTH))
        .flex_none()
        .gap(px(14.))
        .child(panel_heading("metronome", "Tempo and meter"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(13.))
                .text_color(theme::text_muted())
                .child(icon("flag", theme::text_faint(), 14.))
                .child(
                    div()
                        .min_w_0()
                        .font_family(theme::MONO)
                        .child(one_line_text(start)),
                ),
        )
        .child(
            div()
                .id("changes")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(10.))
                .when(view.changes.is_empty(), |list| {
                    list.child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme::text_muted())
                            .child("No changes: the song keeps this tempo and meter."),
                    )
                })
                .children(view.changes.iter().map(|change| change_row(change, cx))),
        )
        .child(
            div()
                .pt(px(10.))
                .border_t_1()
                .border_color(theme::border())
                .text_size(px(12.))
                .text_color(theme::text_faint())
                .child(
                    "A change starts at a bar, counted from 1; leave Tempo or Time empty to keep it.",
                ),
        )
}

fn change_row(change: &ChangeRow, cx: &mut Context<Perform>) -> AnyElement {
    let index = change.index;
    div()
        .flex_none()
        .flex()
        .items_end()
        .gap(px(8.))
        .child(labelled(
            "Bar",
            Some("flag"),
            field(
                format!("change-bar-{index}"),
                &change.bar,
                SongField::ChangeBar(index),
                SHORT,
                "",
                cx,
            ),
        ))
        .child(labelled(
            "Tempo",
            Some("metronome"),
            field(
                format!("change-bpm-{index}"),
                &change.bpm,
                SongField::ChangeBpm(index),
                NUMBER,
                "same",
                cx,
            ),
        ))
        .child(labelled(
            "Time",
            None,
            field(
                format!("change-time-{index}"),
                &change.time,
                SongField::ChangeTime(index),
                SHORT,
                "same",
                cx,
            ),
        ))
        .child(div().flex_1())
        .child(
            button(
                ("remove-change", index),
                cx,
                act(ImportAction::RemoveChange(index)),
            )
            .size(px(30.))
            .justify_center()
            .rounded(px(7.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .tooltip(hint("Remove this change"))
            .child(icon("close", theme::text_muted(), 14.)),
        )
        .into_any_element()
}

/// How many stem rows the stems panel has room for before "Show N more",
/// in a window whose content is `viewport`, under `notes` lines of notes.
fn stems_that_fit(view: &SongEditorView, viewport: Size<Pixels>, notes: usize) -> usize {
    let notes = if notes == 0 {
        0.
    } else {
        (20. + notes as f32 * NOTE_LINE).min(NOTES_MAX) + GAP
    };
    let refused = if view.refused.is_empty() {
        0.
    } else {
        PANEL_GAP + 20. + view.refused.len() as f32 * 38.
    };
    let around = 2. * SCREEN_PADDING
        + HEADER_HEIGHT
        + GAP
        + notes
        + 2. * PANEL_PADDING
        // The heading, the source line and "Show N more", with their gaps.
        + 24.
        + PANEL_GAP
        + 16.
        + PANEL_GAP
        + PANEL_GAP
        + SHOW_MORE
        + refused;
    let room = f32::from(viewport.height) - around;
    ((room / STEM_ROW).floor().max(3.)) as usize
}

/// The stems, with their bus buttons and the files the import refuses.
fn stems_panel(view: &SongEditorView, fit: usize, cx: &mut Context<Perform>) -> impl IntoElement {
    // One more row takes the room of the button that would show it.
    let collapsed = !view.all_stems && view.stems.len() > fit + 1;
    let shown = if collapsed { fit } else { view.stems.len() };
    let menu = view.part_menu.as_ref();
    panel()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .child(panel_heading("stems", "Stems"))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child(view.stems_summary.clone()),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::text_faint())
                .child(one_line_text(format!("From {}", view.source))),
        )
        .child(
            div()
                .id("stems")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(view.stems.iter().take(shown).map(|row| {
                    let open = menu.filter(|menu| menu.stem == row.index);
                    stem_row(row, open, cx)
                })),
        )
        .when(collapsed, |panel| {
            panel.child(
                button("show-all-stems", cx, act(ImportAction::ShowAllStems))
                    .h(px(SHOW_MORE))
                    .justify_center()
                    .gap(px(6.))
                    .rounded(px(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(theme::chip_border())
                    .text_size(px(13.))
                    .text_color(theme::text_muted())
                    .child(icon("down", theme::text_muted(), 14.))
                    .child(format!("Show {} more", view.stems.len() - shown)),
            )
        })
        .when(!view.refused.is_empty(), |panel| {
            panel.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::danger())
                            .child(icon("close", theme::danger(), 14.))
                            .child("Cannot import"),
                    )
                    .children(view.refused.iter().map(|file| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::danger_text())
                                    .child(one_line_text(file.file.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text_muted())
                                    .child(file.reason.clone()),
                            )
                    })),
            )
        })
}

/// A stem: its icon in its bus's color, its file, AUTO while the import
/// chose its part, and its bus button; its part menu under it when open.
fn stem_row(row: &StemRow, menu: Option<&PartMenu>, cx: &mut Context<Perform>) -> AnyElement {
    let color = theme::bus(row.bus);
    let index = row.index;
    let bus_button = button(
        ("stem-bus", index),
        cx,
        act(ImportAction::TogglePartMenu(index)),
    )
    .w(px(112.))
    .h(px(28.))
    .px(px(8.))
    .gap(px(6.))
    .rounded(px(7.))
    .bg(theme::raised())
    .border_1()
    .border_color(if row.menu_open {
        theme::accent()
    } else {
        theme::border()
    })
    .text_size(px(12.))
    .tooltip(hint(format!("{} · {}", row.part.name(), row.bus.name())))
    .child(div().size(px(8.)).flex_none().rounded(px(2.)).bg(color))
    .child(
        div()
            .flex_1()
            .min_w_0()
            .child(one_line_text(row.bus.name())),
    );
    div()
        .flex_none()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(36.))
                .border_b_1()
                .border_color(theme::divider())
                .child(icon(row.icon, color, 18.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(one_line_text(row.file.clone())),
                )
                .when(row.automatic, |line| {
                    line.child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_muted())
                            .child("AUTO"),
                    )
                })
                .child(bus_button),
        )
        .when_some(menu, |row, menu| row.child(part_menu(menu, cx)))
        .into_any_element()
}

/// The column of the part menu a bus's parts go in: the small buses share
/// the first and last columns, Keys & Pads has the middle one to itself.
fn menu_column(bus: Bus) -> usize {
    match bus {
        Bus::ClickGuide | Bus::Drums | Bus::Bass => 0,
        Bus::KeysPads => 1,
        Bus::Guitars | Bus::Vocals => 2,
    }
}

/// Every part, under its bus's heading; the stem's own is checked.
fn part_menu(menu: &PartMenu, cx: &mut Context<Perform>) -> impl IntoElement {
    let column = |number: usize, cx: &mut Context<Perform>| {
        div()
            .w(px(MENU_COLUMN))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(10.))
            .children(
                menu.groups
                    .iter()
                    .filter(|group| menu_column(group.bus) == number)
                    .map(|group| part_group(menu.stem, group, cx))
                    .collect::<Vec<_>>(),
            )
    };
    div()
        .flex()
        .flex_wrap()
        .gap(px(16.))
        .my(px(6.))
        .p(px(12.))
        .rounded(px(10.))
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .child(column(0, cx))
        .child(column(1, cx))
        .child(column(2, cx))
}

fn part_group(stem: usize, group: &PartGroup, cx: &mut Context<Perform>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .pb(px(2.))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(
                    div()
                        .size(px(8.))
                        .flex_none()
                        .rounded(px(2.))
                        .bg(theme::bus(group.bus)),
                )
                .child(group.bus.name()),
        )
        .children(group.parts.iter().map(|choice| {
            let (bg, fg) = if choice.selected {
                (theme::selected(), theme::text())
            } else {
                (theme::panel(), theme::text_muted())
            };
            button(
                gpui::ElementId::Name(format!("part-{stem}-{}", choice.part.id()).into()),
                cx,
                act(ImportAction::SetPart(stem, choice.part)),
            )
            .h(px(28.))
            .px(px(8.))
            .gap(px(6.))
            .rounded(px(6.))
            .bg(bg)
            .text_size(px(13.))
            .text_color(fg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(one_line_text(choice.part.name())),
            )
            .when(choice.selected, |item| {
                item.child(icon("check", theme::accent(), 14.))
            })
        }))
        .into_any_element()
}
