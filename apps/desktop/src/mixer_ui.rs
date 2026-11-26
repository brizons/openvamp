//! Presentational code for the Mixer screen (S3, F7): draws the
//! [`MixerScreen`] view model and sends [`MixerAction`]s to the container.
//! Faders drag with gpui's drag events, so a fast drag keeps following the
//! pointer outside the fader; a double click returns a fader to 0 dB. The
//! engine reports no levels, so there are no meters.

use domain::Bus;
use gpui::{
    AnyElement, ClickEvent, Context, Div, DragMoveEvent, ElementId, FontWeight, IntoElement,
    Render, SharedString, Stateful, Window, div, prelude::*, px, relative,
};
use openvamp_desktop::mixer::{
    BusStrip, FADER_CAP_HEIGHT, FADER_CAP_WIDTH, FADER_PADDING, Layer, MasterStrip, MixerAction,
    MixerScreen, StemStrip, fader_drag_position,
};

use crate::screen::Perform;
use crate::theme;
use crate::ui::OneLine;

/// What a fader sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaderTarget {
    Stem(usize),
    Bus(Bus),
    Master,
}

impl FaderTarget {
    fn moved(self, position: f32) -> MixerAction {
        match self {
            Self::Stem(stem) => MixerAction::StemFader { stem, position },
            Self::Bus(bus) => MixerAction::BusFader { bus, position },
            Self::Master => MixerAction::MasterFader(position),
        }
    }

    fn unity(self) -> MixerAction {
        match self {
            Self::Stem(stem) => MixerAction::StemUnity(stem),
            Self::Bus(bus) => MixerAction::BusUnity(bus),
            Self::Master => MixerAction::MasterUnity,
        }
    }

    fn id(self) -> ElementId {
        ElementId::Name(
            match self {
                Self::Stem(stem) => format!("fader-stem-{stem}"),
                Self::Bus(bus) => format!("fader-bus-{}", bus.id()),
                Self::Master => "fader-master".to_owned(),
            }
            .into(),
        )
    }
}

/// A fader being dragged.
pub(crate) struct FaderDrag(FaderTarget);

/// Nothing follows the pointer while a fader drags: the cap does.
struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A button that sends `action`.
fn action_button(
    id: impl Into<ElementId>,
    cx: &mut Context<Perform>,
    action: MixerAction,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(cx.listener(move |screen, _: &ClickEvent, _, cx| {
            screen.mixer_act(action, cx);
        }))
}

/// The Mixer screen with nothing loaded.
pub fn no_mixer() -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(14.))
        .bg(theme::surface())
        .text_color(theme::text_muted())
        .child("Open a song or a setlist to mix it")
}

/// The Mixer screen.
pub fn mixer(view: &MixerScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let strips: Vec<AnyElement> = match view.layer {
        Layer::Stems => view
            .stems
            .iter()
            .map(|stem| stem_strip(stem, view.bus_menu == Some(stem.index), cx))
            .collect(),
        Layer::Buses => {
            let mut strips: Vec<AnyElement> =
                view.buses.iter().map(|bus| bus_strip(bus, cx)).collect();
            strips.push(master_strip(&view.master, cx));
            strips
        }
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(header(view, cx))
        .children(view.notices.iter().map(|notice| {
            div()
                .flex_none()
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
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(14.))
                .rounded(px(14.))
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .child(toolbar(view, cx))
                .child(
                    div()
                        .id("strips")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .gap(px(6.))
                        .overflow_x_scroll()
                        .children(strips),
                ),
        )
}

fn header(view: &MixerScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let reset = action_button("mixer-reset", cx, MixerAction::Reset)
        .h(px(44.))
        .px(px(14.))
        .rounded(px(10.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(14.))
        .when(!view.can_reset, |button| button.opacity(0.4))
        .child("Reset to song mix");
    let save = action_button("mixer-save", cx, MixerAction::Save)
        .h(px(44.))
        .px(px(14.))
        .rounded(px(10.))
        .font_weight(FontWeight::BOLD)
        .text_size(px(14.))
        .map(|button| {
            if view.can_save {
                button.bg(theme::text()).text_color(theme::on_fill())
            } else {
                button
                    .bg(theme::raised())
                    .text_color(theme::text_faint())
                    .border_1()
                    .border_color(theme::border())
            }
        })
        .child("Save mix to arrangement");
    div()
        .flex_none()
        .h(px(72.))
        .flex()
        .items_center()
        .gap(px(14.))
        .px(px(14.))
        .rounded(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
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
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .child(view.title.clone()),
                )
                .child(
                    div()
                        .one_line()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child(view.arrangement.clone()),
                ),
        )
        .child(reset)
        .child(save)
}

fn toolbar(view: &MixerScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let summary = match view.layer {
        Layer::Stems => view.summary.clone(),
        Layer::Buses => view.outputs.clone(),
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(14.))
        .child(
            div()
                .flex()
                .p(px(3.))
                .rounded(px(10.))
                .bg(theme::bg())
                .child(segment(
                    view.layer,
                    "layer-buses",
                    "Buses",
                    Layer::Buses,
                    cx,
                ))
                .child(segment(
                    view.layer,
                    "layer-stems",
                    "Stems",
                    Layer::Stems,
                    cx,
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .one_line()
                .text_size(px(14.))
                .text_color(theme::text_muted())
                .child(summary),
        )
        .when(view.layer == Layer::Stems, |toolbar| {
            toolbar.child(div().flex().gap(px(10.)).children(Bus::ALL.map(|bus| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(div().size(px(10.)).rounded(px(3.)).bg(theme::bus(bus)))
                    .child(bus.name())
            })))
        })
}

/// One side of the Buses | Stems control.
fn segment(
    shown: Layer,
    id: &'static str,
    label: &'static str,
    layer: Layer,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let on = shown == layer;
    action_button(id, cx, MixerAction::ShowLayer(layer))
        .h(px(30.))
        .px(px(14.))
        .rounded(px(8.))
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .when(on, |item| {
            item.bg(theme::segment()).text_color(theme::text())
        })
        .when(!on, |item| item.text_color(theme::text_muted()))
        .child(label)
}

/// A strip's frame: the bus color bar on top.
fn strip_frame(color: gpui::Rgba, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .pb(px(10.))
        .rounded(px(10.))
        .bg(theme::strip())
        .border_1()
        .border_color(theme::strip_border())
        .overflow_hidden()
        .child(div().w_full().h(px(5.)).bg(color))
}

fn stem_strip(stem: &StemStrip, menu: bool, cx: &mut Context<Perform>) -> AnyElement {
    let index = stem.index;
    let chip = action_button(
        ElementId::Name(format!("bus-chip-{index}").into()),
        cx,
        MixerAction::OpenBusMenu(index),
    )
    .h(px(24.))
    .max_w(px(80.))
    .px(px(6.))
    .rounded(px(6.))
    .bg(theme::raised())
    .border_1()
    .border_color(if menu {
        theme::accent()
    } else {
        theme::border()
    })
    .text_size(px(11.))
    .text_color(theme::text_muted())
    .child(div().min_w_0().one_line().child(stem.bus.name()));
    let body: AnyElement = if menu {
        bus_choices(stem, cx).into_any_element()
    } else {
        fader(FaderTarget::Stem(index), stem.fader, false, cx)
            .when(stem.silenced, |fader| fader.opacity(0.4))
            .into_any_element()
    };
    strip_frame(theme::bus(stem.bus), 84.)
        .child(
            div()
                .w_full()
                .px(px(6.))
                .one_line()
                .text_size(px(14.))
                .font_weight(FontWeight::BOLD)
                .flex()
                .justify_center()
                .child(stem.name.clone()),
        )
        .child(chip)
        .child(body)
        .child(readout(&stem.db))
        .child(
            div()
                .flex()
                .gap(px(6.))
                .child(toggle(
                    ElementId::Name(format!("mute-{index}").into()),
                    "M",
                    stem.mute,
                    theme::text(),
                    MixerAction::ToggleStemMute(index),
                    cx,
                ))
                .child(toggle(
                    ElementId::Name(format!("solo-{index}").into()),
                    "S",
                    stem.solo,
                    theme::solo(),
                    MixerAction::ToggleStemSolo(index),
                    cx,
                )),
        )
        .into_any_element()
}

/// A stem's bus choices: the rules table's ("Default") and every bus.
fn bus_choices(stem: &StemStrip, cx: &mut Context<Perform>) -> impl IntoElement {
    let index = stem.index;
    div()
        .flex_1()
        .w_full()
        .px(px(6.))
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(bus_choice(
            index,
            format!("bus-default-{index}"),
            "Default",
            None,
            stem.default_bus,
            cx,
        ))
        .children(Bus::ALL.map(|bus| {
            bus_choice(
                index,
                format!("bus-{}-{index}", bus.id()),
                bus.name(),
                Some(bus),
                !stem.default_bus && stem.bus == bus,
                cx,
            )
        }))
}

/// One of a stem's bus choices.
fn bus_choice(
    stem: usize,
    id: String,
    label: &'static str,
    bus: Option<Bus>,
    on: bool,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    action_button(
        ElementId::Name(id.into()),
        cx,
        MixerAction::StemBus { stem, bus },
    )
    .w_full()
    .h(px(26.))
    .rounded(px(6.))
    .text_size(px(11.))
    .when(on, |item| {
        item.bg(theme::segment()).text_color(theme::text())
    })
    .when(!on, |item| item.text_color(theme::text_muted()))
    .child(div().min_w_0().one_line().child(label))
}

fn bus_strip(bus: &BusStrip, cx: &mut Context<Perform>) -> AnyElement {
    strip_frame(theme::bus(bus.bus), 96.)
        .child(
            div()
                .w_full()
                .px(px(6.))
                .one_line()
                .flex()
                .justify_center()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(bus.name.to_uppercase())),
        )
        .child(output_label(&bus.output))
        .child(
            fader(FaderTarget::Bus(bus.bus), bus.fader, false, cx)
                .when(bus.mute, |fader| fader.opacity(0.45)),
        )
        .child(readout(&bus.db))
        .child(toggle(
            ElementId::Name(format!("bus-mute-{}", bus.bus.id()).into()),
            "M",
            bus.mute,
            theme::text(),
            MixerAction::ToggleBusMute(bus.bus),
            cx,
        ))
        .into_any_element()
}

fn master_strip(master: &MasterStrip, cx: &mut Context<Perform>) -> AnyElement {
    strip_frame(theme::text(), 96.)
        .bg(theme::raised())
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("MASTER"),
        )
        .child(output_label(&master.output))
        .child(fader(FaderTarget::Master, master.fader, true, cx))
        .child(readout(&master.db))
        .child(div().h(px(34.)))
        .into_any_element()
}

fn output_label(output: &str) -> impl IntoElement {
    div()
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .child(output.to_owned())
}

fn readout(db: &str) -> impl IntoElement {
    div()
        .font_family(theme::MONO)
        .text_size(px(12.))
        .child(db.to_owned())
}

/// An M or S button: lit in `color` when on.
fn toggle(
    id: ElementId,
    label: &'static str,
    on: bool,
    color: gpui::Rgba,
    action: MixerAction,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    action_button(id, cx, action)
        .size(px(30.))
        .rounded(px(7.))
        .text_size(px(13.))
        .font_weight(FontWeight::BOLD)
        .map(|button| {
            if on {
                button.bg(color).text_color(theme::on_fill())
            } else {
                button
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .text_color(theme::text_muted())
            }
        })
        .child(label)
}

/// A vertical fader for `target` with its cap at `position` (0.0 at the
/// bottom, silent, to 1.0 at the top): dragging moves it and sends the new
/// level as it goes; a double click returns it to 0 dB.
fn fader(
    target: FaderTarget,
    position: f32,
    master: bool,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let cap = if master {
        theme::accent()
    } else {
        theme::fader_cap()
    };
    div()
        .id(target.id())
        .flex_1()
        .min_h(px(120.))
        .w(px(36.))
        // Room for the cap at both ends.
        .py(px(FADER_PADDING))
        .cursor_pointer()
        .on_drag(FaderDrag(target), |_, _, _, cx| cx.new(|_| NoPreview))
        .on_drag_move(
            cx.listener(move |screen, event: &DragMoveEvent<FaderDrag>, _, cx| {
                if event.drag(cx).0 != target {
                    return;
                }
                let position = fader_drag_position(
                    f32::from(event.bounds.origin.y),
                    f32::from(event.bounds.size.height),
                    f32::from(event.event.position.y),
                );
                screen.mixer_act(target.moved(position), cx);
            }),
        )
        .on_click(cx.listener(move |screen, event: &ClickEvent, _, cx| {
            if event.click_count() == 2 {
                screen.mixer_act(target.unity(), cx);
            }
        }))
        .child(
            div()
                .relative()
                .size_full()
                .flex()
                .justify_center()
                .child(div().w(px(4.)).h_full().rounded(px(2.)).bg(theme::border()))
                .child(
                    div()
                        .absolute()
                        .top(relative(1.0 - position.clamp(0.0, 1.0)))
                        .mt(px(-FADER_PADDING))
                        .w(px(FADER_CAP_WIDTH))
                        .h(px(FADER_CAP_HEIGHT))
                        .rounded(px(4.))
                        .bg(cap),
                ),
        )
}
