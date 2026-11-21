//! Presentational code for the Audio output screen (S3, F8): draws the
//! [`OutputScreen`] view model and sends [`OutputAction`]s to the
//! container, after the design canvas's Settings artboard. Its MIDI section
//! is out of scope (F9) and not drawn, so the title leaves MIDI out.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, IntoElement, Rgba, Stateful, div,
    prelude::*, px,
};
use openvamp_desktop::output_page::{DeviceState, OutputAction, OutputScreen, RoutingRow};

use crate::screen::Perform;
use crate::theme;
use crate::ui::{OneLine, icon};

/// The width of the matrix's bus column.
const BUS_COLUMN: f32 = 150.;
/// The narrowest a matrix cell gets, before the matrix scrolls sideways.
const CELL_MIN: f32 = 30.;
/// The gap between the matrix's columns.
const MATRIX_GAP: f32 = 4.;
/// The padding at each side of a matrix row.
const ROW_PADDING: f32 = 6.;

/// A button that sends `action`.
fn action_button(
    id: impl Into<ElementId>,
    cx: &mut Context<Perform>,
    action: OutputAction,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .cursor_pointer()
        .on_click(cx.listener(move |screen, _: &ClickEvent, _, cx| {
            screen.output_act(action, cx);
        }))
}

/// A panel of the screen: the canvas's section card.
fn panel(gap: f32) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(gap))
        .p(px(16.))
        .rounded(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
}

/// A panel's title with its icon.
fn heading(icon_name: &'static str, title: &'static str) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .text_size(px(17.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon(icon_name, theme::text_muted(), 20.))
        .child(title)
}

/// A field's label, with an icon when the canvas gives it one.
fn field_label(icon_name: Option<&'static str>, text: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(13.))
        .text_color(theme::text_muted())
        .children(icon_name.map(|name| icon(name, theme::text_muted(), 14.)))
        .child(text)
}

/// A small bordered button of the Routing panel's header.
fn header_button(button: Stateful<Div>) -> Stateful<Div> {
    button
        .flex_none()
        .h(px(32.))
        .px(px(10.))
        .gap(px(6.))
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .whitespace_nowrap()
}

/// The Audio output screen.
pub fn output(view: &OutputScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .id("output-screen")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .h(px(44.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(26.))
                        .font_weight(FontWeight::BOLD)
                        .child("Audio output"),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(theme::text_muted())
                        .child("Saved per device · changes apply instantly"),
                )
                .when(view.applying, |row| {
                    row.child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::queued())
                            .child("Applying…"),
                    )
                }),
        )
        .children(view.notices.iter().map(|notice| {
            div()
                .px(px(14.))
                .py(px(8.))
                .rounded(px(10.))
                .bg(theme::danger_bg())
                .text_color(theme::danger_text())
                .text_size(px(13.))
                .child(notice.clone())
        }))
        .child(
            div()
                .flex()
                .gap(px(12.))
                .child(device_panel(view, cx))
                .child(routing_panel(view, cx)),
        )
}

/// The device's icon with the dot that says whether it plays the song
/// ([`DeviceState`]); no dot with nothing open.
fn device_icon(state: DeviceState) -> Div {
    let dot: Option<Rgba> = match state {
        DeviceState::Open => Some(theme::accent()),
        DeviceState::Lost => Some(theme::danger()),
        DeviceState::Closed => None,
    };
    div()
        .relative()
        .flex_none()
        .flex()
        .child(icon("device", theme::text_soft(), 22.))
        .children(dot.map(|color| {
            // An 8 px dot ringed in the button's background, as the
            // canvas's 2 px shadow.
            div()
                .absolute()
                .top(px(0.))
                .right(px(-5.))
                .size(px(12.))
                .rounded(px(6.))
                .border_2()
                .border_color(theme::bg())
                .bg(color)
        }))
}

fn device_panel(view: &OutputScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let device = action_button("device-button", cx, OutputAction::OpenDeviceMenu)
        .h(px(44.))
        .px(px(12.))
        .gap(px(10.))
        .rounded(px(10.))
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border())
        .text_size(px(15.))
        .child(device_icon(view.device_state))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .one_line()
                .child(view.device.clone()),
        )
        .child(icon("down", theme::text(), 16.));
    let menu = view.device_menu.then(|| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(4.))
            .rounded(px(10.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .children(view.devices.iter().enumerate().map(|(index, device)| {
                action_button(
                    ElementId::Name(format!("device-{index}").into()),
                    cx,
                    OutputAction::ChooseDevice(index),
                )
                .h(px(36.))
                .px(px(10.))
                .rounded(px(8.))
                .text_size(px(14.))
                .when(device.chosen, |row| row.bg(theme::selected()))
                .child(div().min_w_0().one_line().child(device.label.clone()))
            }))
    });
    let rate = match &view.sample_rate_note {
        Some(note) => div()
            .flex()
            .items_baseline()
            .gap(px(4.))
            .child(
                div()
                    .font_family(theme::MONO)
                    .text_size(px(15.))
                    .child(view.sample_rate.clone()),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme::text_muted())
                    .child(format!("· {note}")),
            ),
        None => div()
            .text_size(px(15.))
            .text_color(theme::text_muted())
            .child(view.sample_rate.clone()),
    };
    let buffers = div()
        .flex()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(10.))
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border())
        .children(view.buffer_sizes.iter().map(|&frames| {
            let on = frames == view.buffer;
            action_button(
                ElementId::Name(format!("buffer-{frames}").into()),
                cx,
                OutputAction::Buffer(frames),
            )
            .flex_1()
            .min_w_0()
            .h(px(36.))
            .justify_center()
            .rounded(px(7.))
            .font_family(theme::MONO)
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .when(on, |item| {
                item.bg(theme::selected()).text_color(theme::text())
            })
            .when(!on, |item| item.text_color(theme::text_muted()))
            .child(frames.to_string())
        }));
    panel(16.)
        .w(px(400.))
        .flex_none()
        .child(heading("device", "Audio device"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field_label(None, "Output device"))
                .child(device)
                .children(menu),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field_label(Some("sine"), "Sample rate"))
                .child(rate),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(field_label(Some("clock"), "Buffer size"))
                        .child(div().flex_1())
                        .child(
                            div()
                                .font_family(theme::MONO)
                                .text_size(px(14.))
                                .child(view.latency.clone()),
                        ),
                )
                .child(buffers)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child("Smaller is tighter but needs a faster device."),
                ),
        )
        .children(view.split_mode.map(|on| split_mode(on, cx)))
}

fn split_mode(on: bool, cx: &mut Context<Perform>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .pt(px(14.))
        .border_t_1()
        .border_color(theme::border())
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(icon("headphones", theme::text_muted(), 18.))
                        .child("Split mode"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child("For 2-output devices: click and guide on the left, music summed on the right."),
                ),
        )
        .child(
            action_button("split-mode", cx, OutputAction::ToggleSplit)
                .w(px(48.))
                .h(px(28.))
                .flex_none()
                .p(px(3.))
                .rounded(px(14.))
                .bg(if on { theme::accent() } else { theme::chip_border() })
                .when(on, |switch| switch.justify_end())
                .child(div().size(px(22.)).rounded(px(11.)).bg(theme::text())),
        )
}

fn routing_panel(view: &OutputScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let preset = header_button(action_button(
        "preset-button",
        cx,
        OutputAction::OpenPresetMenu,
    ))
    .child(icon("output-ports", theme::text_faint(), 14.))
    .child(view.preset.clone())
    .child(icon("down", theme::text(), 14.));
    let reset = header_button(action_button("routing-reset", cx, OutputAction::Reset))
        .child(icon("reset", theme::text(), 14.))
        .child("Reset");
    let presets = view.preset_menu.then(|| {
        div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(px(6.))
            .children(view.presets.iter().map(|(outputs, text)| {
                header_button(action_button(
                    ElementId::Name(format!("preset-{outputs}").into()),
                    cx,
                    OutputAction::Preset(*outputs),
                ))
                .child(text.clone())
            }))
    });
    let outputs = view.outputs;
    // The canvas's `minmax(30px, 1fr)` columns: below this width the
    // matrix scrolls sideways instead of squeezing its cells.
    let min_width = BUS_COLUMN + f32::from(outputs) * (CELL_MIN + MATRIX_GAP) + 2. * ROW_PADDING;
    let header = div()
        .flex()
        .flex_none()
        .min_w(px(min_width))
        .gap(px(MATRIX_GAP))
        .px(px(ROW_PADDING))
        .pb(px(4.))
        .child(
            div()
                .w(px(BUS_COLUMN))
                .flex_none()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child("Bus / output"),
        )
        .children((1..=outputs).map(|output| {
            div()
                .flex_1()
                .min_w(px(CELL_MIN))
                .flex()
                .justify_center()
                .font_family(theme::MONO)
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(output.to_string())
        }));
    let rows: Vec<AnyElement> = view
        .rows
        .iter()
        .map(|row| matrix_row(row, min_width, cx))
        .collect();
    panel(10.)
        .flex_1()
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(heading("routing", "Routing"))
                .child(
                    div()
                        .min_w_0()
                        .one_line()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child(view.hint),
                )
                .child(div().flex_1())
                .child(preset)
                .child(reset),
        )
        .children(presets)
        .child(
            div()
                .id("matrix")
                .overflow_x_scroll()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(header)
                .children(rows),
        )
}

fn matrix_row(row: &RoutingRow, min_width: f32, cx: &mut Context<Perform>) -> AnyElement {
    let bus = row.bus;
    div()
        .flex()
        .flex_none()
        .min_w(px(min_width))
        .items_center()
        .gap(px(MATRIX_GAP))
        .min_h(px(44.))
        .px(px(ROW_PADDING))
        .rounded(px(8.))
        .bg(theme::strip())
        .child(
            div()
                .w(px(BUS_COLUMN))
                .flex_none()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(icon(row.icon, theme::bus(bus), 18.))
                .child(
                    div()
                        .min_w_0()
                        .one_line()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(row.name),
                ),
        )
        .children(row.cells.iter().enumerate().map(|(index, &on)| {
            let output = index as u16 + 1;
            action_button(
                ElementId::Name(format!("cell-{}-{output}", bus.id()).into()),
                cx,
                OutputAction::Cell { bus, output },
            )
            .flex_1()
            .min_w(px(CELL_MIN))
            .h(px(40.))
            .justify_center()
            .rounded(px(7.))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::strip_border())
            .child(if on {
                div().size(px(12.)).rounded(px(6.)).bg(theme::accent())
            } else {
                div()
                    .size(px(12.))
                    .rounded(px(6.))
                    .border(px(1.5))
                    .border_color(theme::chip_border())
            })
        }))
        .into_any_element()
}
