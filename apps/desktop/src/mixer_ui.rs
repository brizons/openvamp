//! Presentational code for the Mixer screen (S3, F7): draws the
//! [`MixerScreen`] view model after the design canvas's Mixer artboard and
//! sends [`MixerAction`]s to the container. Faders drag with gpui's drag
//! events, so a fast drag keeps following the pointer outside the fader; a
//! double click returns a fader to 0 dB. Every fader has its meter slot
//! beside it, empty: the engine reports no levels yet (T6b fills them).
//! The arrangement chip draws as the design's dropdown but picks nothing
//! yet: the app cannot switch arrangements. The Perform screen shows the
//! buses layer's strips compactly under its pads ([`compact_mixer`]).

use domain::Bus;
use gpui::{
    AnyElement, ClickEvent, Context, Div, DragMoveEvent, ElementId, FontWeight, IntoElement,
    Render, Rgba, SharedString, Stateful, Window, div, prelude::*, px, relative,
};
use openvamp_desktop::action::{Intent, Page, UiAction};
use openvamp_desktop::mixer::{
    BusStrip, FADER_CAP_HEIGHT, FADER_CAP_WIDTH, FADER_PADDING, Layer, MasterStrip, MixerAction,
    MixerScreen, StemStrip, fader_drag_position,
};

use crate::screen::Perform;
use crate::theme;
use crate::ui::{OneLine, button, icon};

/// The narrowest a stem strip gets before the strips scroll; wider
/// windows share the width between them.
const STEM_STRIP_MIN_WIDTH: f32 = 76.0;

/// The narrowest a bus or master strip gets.
const BUS_STRIP_MIN_WIDTH: f32 = 92.0;

/// A fader's width: the cap and some room around it to grab.
const FADER_WIDTH: f32 = 36.0;

/// A stem's meter slot's width.
const STEM_METER_WIDTH: f32 = 5.0;

/// The width of each of a bus's two meter slots (left and right).
const BUS_METER_WIDTH: f32 = 6.0;

/// The opacity of a control that does nothing now.
const DISABLED_OPACITY: f32 = 0.45;

/// The least height of a strip's meters and fader on the Mixer screen.
const FADER_ZONE_MIN_HEIGHT: f32 = 120.0;

/// The least height of a bus strip's meters and fader in the compact mixer
/// on the Perform screen, which gets the room the pads leave.
const COMPACT_FADER_ZONE_MIN_HEIGHT: f32 = 40.0;

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
    let (strips, gap): (Vec<AnyElement>, f32) = match view.layer {
        Layer::Stems => (
            view.stems
                .iter()
                .map(|stem| stem_strip(stem, view.bus_menu == Some(stem.index), cx))
                .collect(),
            6.,
        ),
        Layer::Buses => (bus_strips(view, FADER_ZONE_MIN_HEIGHT, cx), 8.),
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
                        .gap(px(gap))
                        .pb(px(6.))
                        .overflow_x_scroll()
                        .children(strips),
                ),
        )
}

/// A strip per bus and the master's, with meters and faders at least
/// `zone` points tall.
fn bus_strips(view: &MixerScreen, zone: f32, cx: &mut Context<Perform>) -> Vec<AnyElement> {
    let mut strips: Vec<AnyElement> = view
        .buses
        .iter()
        .map(|bus| bus_strip(bus, zone, cx))
        .collect();
    strips.push(master_strip(&view.master, zone, cx));
    strips
}

/// The design canvas's bus mixer at the bottom of the Perform screen: the
/// buses layer's strips, the Buses | Stems control, whose Stems opens the
/// Mixer screen on its stems, and the device's outputs.
pub(crate) fn compact_mixer(view: &MixerScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let segment = |id: &'static str, icon_name: &'static str, label: &'static str, on: bool| {
        let color = if on {
            theme::text()
        } else {
            theme::text_muted()
        };
        div()
            .id(id)
            .h(px(28.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(7.))
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color)
            .when(on, |item| item.bg(theme::segment()))
            .child(icon(icon_name, color, 14.))
            .child(label)
    };
    let stems = segment("perform-layer-stems", "wave", "Stems", false)
        .cursor_pointer()
        .on_click(cx.listener(|screen, _: &ClickEvent, window, cx| {
            screen.mixer_act(MixerAction::ShowLayer(Layer::Stems), cx);
            screen.handle(Intent::ShowPage(Page::Mixer), window, cx);
        }));
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(14.))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .child(
            div()
                .h(px(34.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(2.))
                        .p(px(3.))
                        .rounded(px(10.))
                        .bg(theme::bg())
                        .border_1()
                        .border_color(theme::border())
                        .child(segment("perform-layer-buses", "sliders", "Buses", true))
                        .child(stems),
                )
                .child(div().flex_1())
                .child(outputs(&view.outputs)),
        )
        .child(
            div()
                .id("perform-strips")
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(8.))
                .pb(px(6.))
                .overflow_x_scroll()
                .children(bus_strips(view, COMPACT_FADER_ZONE_MIN_HEIGHT, cx)),
        )
}

/// The device's outputs, such as "8 outputs", with their icon.
fn outputs(outputs: &str) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(13.))
        .text_color(theme::text_muted())
        .child(icon("output-ports", theme::text_muted(), 16.))
        .child(outputs.to_owned())
}

/// The song, its place in the setlist, the arrangement, the time,
/// play/pause and the mix's two buttons.
fn header(view: &MixerScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let reset = action_button("mixer-reset", cx, MixerAction::Reset)
        .flex_none()
        .h(px(44.))
        .px(px(14.))
        .rounded(px(10.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(14.))
        .whitespace_nowrap()
        .when(!view.can_reset, |button| button.opacity(DISABLED_OPACITY))
        .child("Reset to song mix");
    let save = action_button("mixer-save", cx, MixerAction::Save)
        .flex_none()
        .h(px(44.))
        .px(px(14.))
        .rounded(px(10.))
        .bg(theme::text())
        .text_color(theme::on_fill())
        .font_weight(FontWeight::BOLD)
        .text_size(px(14.))
        .whitespace_nowrap()
        .when(!view.can_save, |button| button.opacity(DISABLED_OPACITY))
        .child("Save mix to arrangement");
    // Drawn as the design's dropdown; the app cannot switch arrangements
    // yet, so it picks nothing.
    let arrangement = div()
        .flex_none()
        .h(px(32.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(8.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .whitespace_nowrap()
        .child(view.arrangement.clone())
        .child(icon("down", theme::text(), 14.));
    let play = button("mixer-play", cx, Intent::Ui(UiAction::TogglePlay))
        .w(px(56.))
        .h(px(48.))
        .justify_center()
        .rounded(px(12.))
        .bg(theme::accent())
        .child(icon(
            if view.playing { "pause" } else { "play" },
            theme::on_fill(),
            24.,
        ));
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
                .flex_initial()
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
                .children(view.position.clone().map(|position| {
                    div()
                        .one_line()
                        .text_size(px(13.))
                        .text_color(theme::text_muted())
                        .child(position)
                })),
        )
        .child(arrangement)
        .child(div().flex_1())
        .child(
            div()
                .flex_none()
                .flex()
                .items_baseline()
                .whitespace_nowrap()
                .font_family(theme::MONO)
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(view.time.clone()),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(theme::text_faint())
                        .child(format!("\u{a0}/ {}", view.total)),
                ),
        )
        .child(play)
        .child(reset)
        .child(save)
}

/// The Buses | Stems control, and the stems' summary and bus legend or
/// the device's outputs.
fn toolbar(view: &MixerScreen, cx: &mut Context<Perform>) -> impl IntoElement {
    let toolbar = div().flex_none().flex().items_center().gap(px(12.)).child(
        div()
            .flex_none()
            .flex()
            .gap(px(2.))
            .p(px(3.))
            .rounded(px(10.))
            .bg(theme::bg())
            .border_1()
            .border_color(theme::border())
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
    );
    match view.layer {
        Layer::Stems => toolbar
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .one_line()
                    .text_size(px(14.))
                    .text_color(theme::text_muted())
                    .child(view.summary.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(14.))
                    .children(Bus::ALL.map(|bus| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(div().size(px(10.)).rounded(px(3.)).bg(theme::bus(bus)))
                            .child(bus.name())
                    })),
            ),
        Layer::Buses => toolbar.child(div().flex_1()).child(outputs(&view.outputs)),
    }
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
        .rounded(px(7.))
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .when(on, |item| {
            item.bg(theme::segment()).text_color(theme::text())
        })
        .when(!on, |item| item.text_color(theme::text_muted()))
        .child(label)
}

/// A strip's frame: the bus color bar on top, `bar` points tall. Strips
/// share the row's width equally, never narrower than `min_width`; past
/// that the row scrolls.
fn strip_frame(color: Rgba, min_width: f32, bar: f32) -> Div {
    div()
        .flex_1()
        .min_w(px(min_width))
        .flex()
        .flex_col()
        .items_center()
        .rounded(px(10.))
        .bg(theme::strip())
        .border_1()
        .border_color(theme::strip_border())
        .overflow_hidden()
        .child(div().flex_none().w_full().h(px(bar)).bg(color))
}

/// An empty meter slot `width` points wide, the height of the fader beside
/// it: the engine reports no levels yet, so nothing fills it.
fn meter_slot(width: f32) -> Div {
    div()
        .flex_none()
        .w(px(width))
        .rounded(px(3.))
        .bg(theme::bg())
}

fn stem_strip(stem: &StemStrip, menu: bool, cx: &mut Context<Perform>) -> AnyElement {
    let index = stem.index;
    let color = theme::bus(stem.bus);
    let chip = action_button(
        ElementId::Name(format!("bus-chip-{index}").into()),
        cx,
        MixerAction::OpenBusMenu(index),
    )
    .flex_none()
    .h(px(24.))
    .max_w_full()
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
        fader_zone(3., FADER_ZONE_MIN_HEIGHT)
            .when(stem.silenced, |zone| zone.opacity(0.4))
            .child(meter_slot(STEM_METER_WIDTH))
            .child(fader(FaderTarget::Stem(index), stem.fader, false, cx))
            .into_any_element()
    };
    strip_frame(color, STEM_STRIP_MIN_WIDTH, 5.)
        .gap(px(8.))
        .pb(px(10.))
        .child(icon(stem.icon, color, 24.))
        .child(
            div()
                .w_full()
                .px(px(4.))
                .one_line()
                .text_center()
                .font_family(theme::CONDENSED)
                .text_size(px(14.))
                .font_weight(FontWeight::BOLD)
                .child(stem.name.clone()),
        )
        .child(chip)
        .child(body)
        .child(readout(&stem.db))
        .child(
            div()
                .flex()
                .gap(px(4.))
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

/// The row that holds a strip's meter slots and its fader, as tall as the
/// strip leaves and at least `min_height`; `gap` points between them.
fn fader_zone(gap: f32, min_height: f32) -> Div {
    div().flex_1().min_h(px(min_height)).flex().gap(px(gap))
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

fn bus_strip(bus: &BusStrip, zone: f32, cx: &mut Context<Perform>) -> AnyElement {
    let color = theme::bus(bus.bus);
    bus_frame(color, theme::strip(), theme::strip_border())
        .child(bus_strip_icon(bus.icon, color))
        .child(bus_name(bus.name))
        .child(output_label(&bus.output))
        .child(
            bus_meters_and_fader(FaderTarget::Bus(bus.bus), bus.fader, false, zone, cx)
                .when(bus.mute, |zone| zone.opacity(DISABLED_OPACITY)),
        )
        .child(readout(&bus.db))
        .child(
            action_button(
                ElementId::Name(format!("bus-mute-{}", bus.bus.id()).into()),
                cx,
                MixerAction::ToggleBusMute(bus.bus),
            )
            .flex_none()
            .w(px(48.))
            .h(px(34.))
            .gap(px(4.))
            .rounded(px(8.))
            .font_family(theme::CONDENSED)
            .text_size(px(14.))
            .font_weight(FontWeight::BOLD)
            .map(|button| {
                let color = if bus.mute {
                    theme::on_fill()
                } else {
                    theme::text()
                };
                lit(button, bus.mute, theme::text())
                    .child(icon("mute", color, 14.))
                    .child("M")
            }),
        )
        .into_any_element()
}

fn master_strip(master: &MasterStrip, zone: f32, cx: &mut Context<Perform>) -> AnyElement {
    bus_frame(theme::text(), theme::raised(), theme::chip_border())
        .child(bus_strip_icon("speaker", theme::text()))
        .child(bus_name("Master"))
        .child(output_label(&master.output))
        .child(bus_meters_and_fader(
            FaderTarget::Master,
            master.fader,
            true,
            zone,
            cx,
        ))
        .child(readout(&master.db))
        // The room of a bus's mute, so the faders line up.
        .child(div().flex_none().h(px(34.)))
        .into_any_element()
}

/// A bus or master strip's frame, on `bg` within `border`.
fn bus_frame(color: Rgba, bg: Rgba, border: Rgba) -> Div {
    strip_frame(color, BUS_STRIP_MIN_WIDTH, 4.)
        .gap(px(6.))
        .pb(px(8.))
        .bg(bg)
        .border_color(border)
}

/// A bus or master strip's icon, a little below the bar.
fn bus_strip_icon(name: &'static str, color: Rgba) -> impl IntoElement {
    div().flex_none().mt(px(4.)).child(icon(name, color, 26.))
}

fn bus_name(name: &str) -> impl IntoElement {
    div()
        .w_full()
        .px(px(4.))
        .one_line()
        .text_center()
        .font_family(theme::CONDENSED)
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(SharedString::from(name.to_uppercase()))
}

/// A bus's two meter slots, left and right, and its fader, at least `zone`
/// points tall.
fn bus_meters_and_fader(
    target: FaderTarget,
    position: f32,
    master: bool,
    zone: f32,
    cx: &mut Context<Perform>,
) -> Div {
    fader_zone(7., zone)
        .child(
            div()
                .flex_none()
                .flex()
                .gap(px(3.))
                .child(meter_slot(BUS_METER_WIDTH))
                .child(meter_slot(BUS_METER_WIDTH)),
        )
        .child(fader(target, position, master, cx))
}

fn output_label(output: &str) -> impl IntoElement {
    div()
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .whitespace_nowrap()
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
    color: Rgba,
    action: MixerAction,
    cx: &mut Context<Perform>,
) -> Stateful<Div> {
    let button = action_button(id, cx, action)
        .flex_none()
        .size(px(30.))
        .rounded(px(7.))
        .font_family(theme::CONDENSED)
        .text_size(px(13.))
        .font_weight(FontWeight::BOLD);
    lit(button, on, color).child(label)
}

/// A mute or solo button's look: filled in `color` when on, else raised.
fn lit(button: Stateful<Div>, on: bool, color: Rgba) -> Stateful<Div> {
    if on {
        button.bg(color).text_color(theme::on_fill())
    } else {
        button
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .text_color(theme::text())
    }
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
        .flex_none()
        .w(px(FADER_WIDTH))
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
