//! The design canvas's color tokens and type, and the icons the screen
//! draws, embedded in the binary.

use std::borrow::Cow;

use domain::Bus;
use gpui::{AssetSource, Result, Rgba, SharedString, rgb, rgba};
use openvamp_desktop::view::SectionKind;

/// Window background.
pub fn bg() -> Rgba {
    rgb(0x0D0E11)
}
/// Panels.
pub fn surface() -> Rgba {
    rgb(0x16181D)
}
/// The side navigation and the tiles of played items.
pub fn panel() -> Rgba {
    rgb(0x121418)
}
/// Buttons and cards on a panel.
pub fn raised() -> Rgba {
    rgb(0x1D2026)
}
/// The selected side of a segmented toggle.
pub fn selected() -> Rgba {
    rgb(0x2C3038)
}
/// Borders.
pub fn border() -> Rgba {
    rgb(0x2A2E36)
}
/// The line under the header.
pub fn divider() -> Rgba {
    rgb(0x22252B)
}
/// Main text.
pub fn text() -> Rgba {
    rgb(0xECEEF1)
}
/// The OpenVamp mark's loop (docs/brand).
pub fn brand() -> Rgba {
    rgb(0x2DD4BF)
}
/// Secondary text.
pub fn text_muted() -> Rgba {
    rgb(0xA3AAB5)
}
/// Text on an upcoming item's badge.
pub fn text_soft() -> Rgba {
    rgb(0xC4C9D1)
}
/// Badges of upcoming items; the selected item of the side navigation.
pub fn badge() -> Rgba {
    rgb(0x262A31)
}
/// The dashed border of a transition chip.
pub fn chip_border() -> Rgba {
    rgb(0x3A3F48)
}
/// Tertiary text: units, counts.
pub fn text_faint() -> Rgba {
    rgb(0x8A919C)
}
/// Play and switched-on controls.
pub fn accent() -> Rgba {
    rgb(0x3DDC97)
}
/// The queued jump: Next, the marker, the cancel button.
pub fn queued() -> Rgba {
    rgb(0xFFB547)
}
/// Background of the card of a queued jump.
pub fn queued_bg() -> Rgba {
    rgb(0x221C12)
}
/// Border of the cancel button.
pub fn queued_border() -> Rgba {
    rgb(0x4A3A1E)
}
/// Panic and errors.
pub fn danger() -> Rgba {
    rgb(0xFF6B6B)
}
/// Background of the panic button.
pub fn danger_bg() -> Rgba {
    rgb(0x3A1618)
}
/// Label of the panic button.
pub fn danger_text() -> Rgba {
    rgb(0xFFB3B3)
}
/// "hold" under the panic label.
pub fn danger_hint() -> Rgba {
    rgb(0xFF9A9A)
}
/// The progress of a panic hold.
pub fn danger_fill() -> Rgba {
    rgba(0xFF6B6B73)
}
/// Text on a light or colored fill.
pub fn on_fill() -> Rgba {
    rgb(0x0D0E11)
}

/// The color of a section family on the song map and its pad.
pub fn section(kind: SectionKind) -> Rgba {
    rgb(match kind {
        SectionKind::Neutral => 0x8E99AB,
        SectionKind::Verse => 0x5E9BFF,
        SectionKind::PreChorus => 0x9F86FF,
        SectionKind::Chorus => 0xF06BB0,
        SectionKind::Bridge => 0x38C9B0,
        SectionKind::Tag => 0xC9B458,
    })
}

/// The color of a bus, as the design canvas's mixer gives each family.
pub fn bus(bus: Bus) -> Rgba {
    rgb(match bus {
        Bus::ClickGuide => 0x8E99AB,
        Bus::Drums => 0x5E9BFF,
        Bus::Bass => 0x9F86FF,
        Bus::KeysPads => 0xF06BB0,
        Bus::Guitars => 0x38C9B0,
        Bus::Vocals => 0xC9B458,
    })
}

/// A mixer strip's background.
pub fn strip() -> Rgba {
    rgb(0x121418)
}

/// A mixer strip's border.
pub fn strip_border() -> Rgba {
    rgb(0x22252B)
}

/// A fader's cap.
pub fn fader_cap() -> Rgba {
    rgb(0xE2E5EA)
}

/// A soloed stem's S.
pub fn solo() -> Rgba {
    rgb(0x4EA8FF)
}

/// The chosen item of a segmented control.
pub fn segment() -> Rgba {
    rgb(0x2C3038)
}

/// A section color at `alpha`, for the unplayed part of the song map.
pub fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// Monospace family for times, counts and keys; the system UI font is
/// used for everything else (the canvas's Barlow and JetBrains Mono are not
/// bundled).
pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

/// The icons, by the path [`gpui::svg`] asks for.
pub struct Icons;

const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/back-to-start.svg",
        include_bytes!("../assets/icons/back-to-start.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../assets/icons/check.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    ("icons/copy.svg", include_bytes!("../assets/icons/copy.svg")),
    (
        "icons/crossfade.svg",
        include_bytes!("../assets/icons/crossfade.svg"),
    ),
    ("icons/cue.svg", include_bytes!("../assets/icons/cue.svg")),
    (
        "icons/device.svg",
        include_bytes!("../assets/icons/device.svg"),
    ),
    ("icons/down.svg", include_bytes!("../assets/icons/down.svg")),
    ("icons/fade.svg", include_bytes!("../assets/icons/fade.svg")),
    (
        "icons/folder.svg",
        include_bytes!("../assets/icons/folder.svg"),
    ),
    ("icons/grip.svg", include_bytes!("../assets/icons/grip.svg")),
    (
        "icons/infinite.svg",
        include_bytes!("../assets/icons/infinite.svg"),
    ),
    ("icons/jump.svg", include_bytes!("../assets/icons/jump.svg")),
    (
        "icons/library.svg",
        include_bytes!("../assets/icons/library.svg"),
    ),
    ("icons/link.svg", include_bytes!("../assets/icons/link.svg")),
    ("icons/loop.svg", include_bytes!("../assets/icons/loop.svg")),
    (
        "icons/metronome.svg",
        include_bytes!("../assets/icons/metronome.svg"),
    ),
    (
        "icons/minus.svg",
        include_bytes!("../assets/icons/minus.svg"),
    ),
    (
        "icons/mixer.svg",
        include_bytes!("../assets/icons/mixer.svg"),
    ),
    ("icons/next.svg", include_bytes!("../assets/icons/next.svg")),
    (
        "icons/next-bar.svg",
        include_bytes!("../assets/icons/next-bar.svg"),
    ),
    ("icons/note.svg", include_bytes!("../assets/icons/note.svg")),
    (
        "icons/openvamp-loop.svg",
        include_bytes!("../assets/icons/openvamp-loop.svg"),
    ),
    (
        "icons/openvamp-play.svg",
        include_bytes!("../assets/icons/openvamp-play.svg"),
    ),
    (
        "icons/output.svg",
        include_bytes!("../assets/icons/output.svg"),
    ),
    (
        "icons/panic.svg",
        include_bytes!("../assets/icons/panic.svg"),
    ),
    (
        "icons/pause.svg",
        include_bytes!("../assets/icons/pause.svg"),
    ),
    (
        "icons/pencil.svg",
        include_bytes!("../assets/icons/pencil.svg"),
    ),
    (
        "icons/perform.svg",
        include_bytes!("../assets/icons/perform.svg"),
    ),
    ("icons/play.svg", include_bytes!("../assets/icons/play.svg")),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    (
        "icons/previous.svg",
        include_bytes!("../assets/icons/previous.svg"),
    ),
    (
        "icons/search.svg",
        include_bytes!("../assets/icons/search.svg"),
    ),
    (
        "icons/section-end.svg",
        include_bytes!("../assets/icons/section-end.svg"),
    ),
    (
        "icons/setlist.svg",
        include_bytes!("../assets/icons/setlist.svg"),
    ),
    ("icons/stay.svg", include_bytes!("../assets/icons/stay.svg")),
    ("icons/stop.svg", include_bytes!("../assets/icons/stop.svg")),
    (
        "icons/trash.svg",
        include_bytes!("../assets/icons/trash.svg"),
    ),
    ("icons/up.svg", include_bytes!("../assets/icons/up.svg")),
];

impl AssetSource for Icons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
