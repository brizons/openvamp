//! Presentational code for the Audio output screen (S3, F8): draws the
//! [`OutputScreen`] view model and sends [`OutputAction`]s to the
//! container. The design canvas's MIDI section is out of scope (F9) and not
//! drawn.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, IntoElement, Stateful, div,
    prelude::*, px,
};
use openvamp_desktop::output_page::{OutputAction, OutputScreen, RoutingRow};

use crate::screen::Perform;
use crate::theme;
use crate::ui::{OneLine, icon};

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

fn panel(title: &'static str, icon_name: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(16.))
        .rounded(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(17.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(icon(icon_name, theme::text_muted(), 18.))
                .child(title),
        )
}

fn label(text: &'static str) -> Div {
    div()
        .text_size(px(13.))
        .text_color(theme::text_muted())
        .child(text)
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
        .gap(px(14.))
        .child(
            div()
                .flex()
                .items_end()
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
                .items_start()
                .gap(px(14.))
                .child(device_panel(view, cx))
                .child(routing_panel(view, cx)),
        )
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
        .child(icon("output", theme::text_muted(), 18.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .one_line()
                .text_size(px(15.))
                .child(view.device.clone()),
        )
        .child(icon("down", theme::text_muted(), 16.));
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
                .when(device.chosen, |row| row.bg(theme::segment()))
                .child(div().min_w_0().one_line().child(device.label.clone()))
            }))
    });
    let buffers = div()
        .flex()
        .p(px(3.))
        .gap(px(3.))
        .rounded(px(10.))
        .bg(theme::bg())
        .children(view.buffer_sizes.iter().map(|&frames| {
            let on = frames == view.buffer;
            action_button(
                ElementId::Name(format!("buffer-{frames}").into()),
                cx,
                OutputAction::Buffer(frames),
            )
            .flex_1()
            .h(px(36.))
            .justify_center()
            .rounded(px(8.))
            .font_family(theme::MONO)
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .when(on, |item| {
                item.bg(theme::segment()).text_color(theme::text())
            })
            .when(!on, |item| item.text_color(theme::text_muted()))
            .child(frames.to_string())
        }));
    panel("Audio device", "output")
        .w(px(400.))
        .flex_none()
        .child(label("Output device"))
        .child(device)
        .children(menu)
        .child(label("Sample rate"))
        .child(
            div()
                .font_family(theme::MONO)
                .text_size(px(15.))
                .child(view.sample_rate.clone()),
        )
        .child(
            div()
                .flex()
                .child(label("Buffer size"))
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
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Split mode"),
                )
                .child(
                    div()
                        .text_size(px(13.))
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
    let preset = action_button("preset-button", cx, OutputAction::OpenPresetMenu)
        .h(px(32.))
        .px(px(10.))
        .gap(px(6.))
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .child(view.preset.clone())
        .child(icon("down", theme::text_muted(), 14.));
    let reset = action_button("routing-reset", cx, OutputAction::Reset)
        .h(px(32.))
        .px(px(10.))
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .child("Reset");
    let presets = view.preset_menu.then(|| {
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(view.presets.iter().map(|(outputs, text)| {
                action_button(
                    ElementId::Name(format!("preset-{outputs}").into()),
                    cx,
                    OutputAction::Preset(*outputs),
                )
                .h(px(30.))
                .px(px(10.))
                .rounded(px(8.))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::border())
                .text_size(px(13.))
                .child(text.clone())
            }))
    });
    let outputs = view.outputs;
    let header = div()
        .flex()
        .gap(px(4.))
        .child(
            div()
                .w(px(150.))
                .flex_none()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child("Bus / output"),
        )
        .children((1..=outputs).map(|output| {
            div()
                .flex_1()
                .min_w(px(30.))
                .flex()
                .justify_center()
                .font_family(theme::MONO)
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(output.to_string())
        }));
    let rows: Vec<AnyElement> = view.rows.iter().map(|row| matrix_row(row, cx)).collect();
    panel("Routing", "link")
        .flex_1()
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .one_line()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child(view.hint),
                )
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
                .gap(px(4.))
                .child(header)
                .children(rows),
        )
}

fn matrix_row(row: &RoutingRow, cx: &mut Context<Perform>) -> AnyElement {
    let bus = row.bus;
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .min_h(px(44.))
        .rounded(px(8.))
        .bg(theme::strip())
        .child(
            div()
                .w(px(150.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .child(div().size(px(10.)).rounded(px(3.)).bg(theme::bus(bus)))
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
            .min_w(px(30.))
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
                    .border_1()
                    .border_color(theme::chip_border())
            })
        }))
        .into_any_element()
}
