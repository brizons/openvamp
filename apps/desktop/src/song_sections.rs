//! What the Song editor of a library song shows of its sections, without
//! a window (S1–S4): the "Section markers" panel (a bar ruler, the marker
//! strip and the waveform, one bar per musical bar), the marker inspector
//! and the "Arrangement" panel, after the design canvas's Song editor
//! artboard. [`crate::song_edit`] builds these views; the binary draws them.
//!
//! The panel draws every bar the same width, so it measures the song in
//! bars ([`BarScale`]): a frame inside a bar sits at its share of that
//! bar, and a marker snapped to a beat sits inside its bar.

use std::ops::Range;

use domain::{BarBeat, SongEditor};
use library::Overview;

use crate::library_page::Choice;
use crate::song_import::{FieldView, SongEditorView};
use crate::view::SectionKind;

/// The share of the waveform's height the quietest bar fills, as on the
/// design canvas: 16 %.
pub const WAVE_LOW: f32 = 0.16;

/// The share of the waveform's height the loudest bar fills: 94 %.
pub const WAVE_HIGH: f32 = 0.94;

/// How many bars apart the ruler labels its bars: 1, 9, 17…
pub const RULER_STEP_BARS: u64 = 8;

/// The fewest bars zooming in shows.
pub const MIN_VISIBLE_BARS: u64 = 4;

/// The width of the visible bars, in the units a position across them is
/// given in ([`crate::song_edit::SongAction::PlaceCursor`]): a millionth.
pub const ALONG: u32 = 1_000_000;

/// The song's frames measured in bars of its bar grid, each bar one unit
/// wide: bar `n` (counted from 1) covers `[n - 1, n)`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BarScale<'a> {
    lines: &'a [u64],
    length: u64,
}

impl<'a> BarScale<'a> {
    /// The scale of the song `editor` edits.
    pub(crate) fn of(editor: &'a SongEditor) -> Self {
        Self {
            lines: editor.grid().bar_lines(),
            length: editor.length_frames(),
        }
    }

    /// How many bars the song has, the last one cut by the song end
    /// included.
    pub(crate) fn bars(&self) -> u64 {
        self.lines.len() as u64
    }

    /// The frames of bar `index`, counted from 0.
    pub(crate) fn bar_range(&self, index: usize) -> Range<u64> {
        let start = self.lines.get(index).copied().unwrap_or(self.length);
        let end = self.lines.get(index + 1).copied().unwrap_or(self.length);
        start..end.max(start)
    }

    /// Where frame `at` is, in bars from the song start.
    pub(crate) fn units(&self, at: u64) -> f64 {
        if at >= self.length || self.lines.is_empty() {
            return self.bars() as f64;
        }
        let index = self
            .lines
            .partition_point(|&line| line <= at)
            .saturating_sub(1);
        let range = self.bar_range(index);
        let width = (range.end - range.start).max(1);
        index as f64 + (at.saturating_sub(range.start)) as f64 / width as f64
    }

    /// The frame `units` bars from the song start, inside the song.
    pub(crate) fn frames(&self, units: f64) -> u64 {
        if self.lines.is_empty() || units <= 0.0 {
            return 0;
        }
        let last = self.lines.len() - 1;
        let index = (units.floor() as usize).min(last);
        let share = (units - index as f64).clamp(0.0, 1.0);
        let range = self.bar_range(index);
        let at = range.start + ((range.end - range.start) as f64 * share).round() as u64;
        at.min(self.length.saturating_sub(1))
    }
}

/// The bars the markers panel shows: from bar `first` (counted from 0),
/// `visible` of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) first: u64,
    pub(crate) visible: u64,
}

impl Window {
    /// Where `units` bars from the song start are across the window, from
    /// 0 (its left edge) to 1 (its right edge).
    fn share(&self, units: f64) -> f32 {
        ((units - self.first as f64) / self.visible.max(1) as f64) as f32
    }

    /// The part of `units` the window shows, across it; `None` when it
    /// shows none of it.
    fn span(&self, units: Range<f64>) -> Option<Span> {
        let start = units.start.max(self.first as f64);
        let end = units.end.min((self.first + self.visible) as f64);
        (end > start).then(|| Span {
            left: self.share(start),
            width: self.share(end) - self.share(start),
        })
    }

    /// The bars `along` (out of [`ALONG`]) across the window are from the
    /// song start.
    pub(crate) fn units_at(&self, along: u32) -> f64 {
        self.first as f64 + f64::from(along.min(ALONG)) / f64::from(ALONG) * self.visible as f64
    }
}

/// "Bar 9" for a position that starts a bar, "Bar 9.3" for its third beat.
pub(crate) fn bar_label(position: BarBeat) -> String {
    match position.beat {
        1 => format!("Bar {}", position.bar),
        beat => format!("Bar {}.{beat}", position.bar),
    }
}

/// "1 bar", "16 bars".
pub(crate) fn bars_text(bars: u64) -> String {
    match bars {
        1 => "1 bar".to_owned(),
        bars => format!("{bars} bars"),
    }
}

/// The bars of the waveform in `window`, one per musical bar, each in the
/// color of the section it starts in: as tall as its level in `overview`
/// over the song's peak, between [`WAVE_LOW`] and [`WAVE_HIGH`]; all at
/// [`WAVE_LOW`] until the overview is there, or for a silent song.
pub(crate) fn wave_bars(
    editor: &SongEditor,
    overview: Option<&Overview>,
    window: Window,
) -> Vec<WaveBar> {
    let scale = BarScale::of(editor);
    let sections = &editor.package().sections;
    let last = scale.bars().min(window.first + window.visible);
    (window.first..last)
        .map(|index| {
            let range = scale.bar_range(index as usize);
            let section = sections
                .partition_point(|marker| marker.start <= range.start)
                .saturating_sub(1);
            let kind = sections
                .get(section)
                .map_or(SectionKind::Neutral, |marker| {
                    SectionKind::of(&marker.section_type.label(marker.number))
                });
            let height = overview.map_or(WAVE_LOW, |overview| {
                bar_height(overview, range, WAVE_LOW..WAVE_HIGH)
            });
            WaveBar { kind, height }
        })
        .collect()
}

/// How tall a waveform's bar of the source frames `frames` stands, as a
/// share of the waveform's height within `heights`: its level in
/// `overview` over the song's peak, from `heights.start` for silence to
/// `heights.end` for the peak; `heights.start` for a silent song. The
/// Song editor draws its bars in [`WAVE_LOW`]..[`WAVE_HIGH`], the Perform
/// song map in [`crate::view::MAP_WAVE_LOW`]..[`crate::view::MAP_WAVE_HIGH`].
pub(crate) fn bar_height(overview: &Overview, frames: Range<u64>, heights: Range<f32>) -> f32 {
    let peak = overview.peak();
    if peak > 0.0 {
        let level = (overview.range_level(frames) / peak).clamp(0.0, 1.0);
        heights.start + (heights.end - heights.start) * level
    } else {
        heights.start
    }
}

/// The ruler's labels in `window`: bars 1, 9, 17… ([`RULER_STEP_BARS`]).
pub(crate) fn ruler(window: Window) -> Vec<RulerMark> {
    let first = window.first.div_ceil(RULER_STEP_BARS) * RULER_STEP_BARS;
    (first..window.first + window.visible)
        .step_by(RULER_STEP_BARS as usize)
        .map(|index| RulerMark {
            bar: index + 1,
            left: window.share(index as f64),
        })
        .collect()
}

/// The part of the frames `range` the window shows, across it.
pub(crate) fn frames_span(editor: &SongEditor, window: Window, range: Range<u64>) -> Option<Span> {
    let scale = BarScale::of(editor);
    window.span(scale.units(range.start)..scale.units(range.end))
}

/// Where frame `at` is across the window, when it shows it.
pub(crate) fn frame_share(editor: &SongEditor, window: Window, at: u64) -> Option<f32> {
    let units = BarScale::of(editor).units(at);
    let share = window.share(units);
    (0.0..=1.0).contains(&share).then_some(share)
}

/// A part of the visible bars, as shares of their width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// Where it starts, from 0 (the left edge) to 1.
    pub left: f32,
    /// How wide it is.
    pub width: f32,
}

/// A bar number on the ruler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RulerMark {
    /// The bar, counted from 1.
    pub bar: u64,
    /// Where its line is across the visible bars.
    pub left: f32,
}

/// A section's block on the marker strip, as wide as the bars it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerBlock {
    /// Its marker, counted from 0 in song order; selects it.
    pub index: usize,
    /// Its label, such as "Verse 1".
    pub label: String,
    /// Its abbreviation, such as "V1".
    pub abbr: String,
    /// Its icon.
    pub icon: &'static str,
    /// Its family, which picks its color.
    pub kind: SectionKind,
    /// Where it starts, such as "Bar 9".
    pub starts: String,
    /// The part of the visible bars it covers.
    pub span: Span,
    /// Whether it is the section the inspector shows.
    pub selected: bool,
    /// Whether its marker can be dragged: every one but the first.
    pub movable: bool,
}

/// One bar of the waveform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveBar {
    /// The family of the section it starts in, which picks its color.
    pub kind: SectionKind,
    /// Its height, as a share of the waveform's, from [`WAVE_LOW`] to
    /// [`WAVE_HIGH`].
    pub height: f32,
}

/// Where a dragged marker would land, while it is dragged.
#[derive(Debug, Clone, PartialEq)]
pub struct DragGhost {
    /// The marker dragged.
    pub index: usize,
    /// Where it would land across the visible bars.
    pub left: f32,
    /// Where that is, such as "Bar 17".
    pub label: String,
}

/// How far the waveform is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveformState {
    /// It is drawn from the song's overview.
    Ready,
    /// The overview is being read, or built from the stems: this much of
    /// it, 0 to 100. The bars are flat meanwhile.
    Reading {
        /// How much is read.
        percent: u32,
    },
    /// The overview could not be built; the bars stay flat.
    Failed(String),
}

/// The "Section markers" panel.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkersView {
    /// Whether "Snap to bar" is on; off, markers snap to the nearest beat.
    pub snap_to_bar: bool,
    /// Whether zooming in shows fewer bars.
    pub can_zoom_in: bool,
    /// Whether zooming out shows more bars.
    pub can_zoom_out: bool,
    /// The first bar shown, counted from 1.
    pub first_bar: u64,
    /// How many bars show.
    pub visible_bars: u64,
    /// How many bars the song has.
    pub total_bars: u64,
    /// The ruler's labels.
    pub ruler: Vec<RulerMark>,
    /// The sections' blocks that show, in song order.
    pub blocks: Vec<MarkerBlock>,
    /// The waveform, one bar per bar shown.
    pub bars: Vec<WaveBar>,
    /// The selected section, where it shows.
    pub selection: Option<Span>,
    /// The cursor ("playhead") where markers are added, where it shows.
    pub cursor: Option<f32>,
    /// Where the cursor is, such as "Bar 9".
    pub cursor_label: String,
    /// A marker being dragged.
    pub drag: Option<DragGhost>,
    /// How far the waveform is.
    pub waveform: WaveformState,
}

/// The marker inspector: the selected section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorView {
    /// Its marker, counted from 0.
    pub index: usize,
    /// Its label, such as "Verse 1".
    pub name: String,
    /// Its icon.
    pub icon: &'static str,
    /// Its family, which picks its color.
    pub kind: SectionKind,
    /// Its type, such as "Verse".
    pub section_type: String,
    /// The type menu, while open: every built-in type
    /// ([`domain::SectionType::ALL`]), its own selected.
    pub type_menu: Option<Vec<Choice>>,
    /// Its number; empty for none.
    pub number: FieldView,
    /// Where it starts, such as "Bar 9".
    pub starts_at: String,
    /// How long it is, such as "16 bars".
    pub length: String,
    /// The name of the arrangement shown, which the loop switch is for.
    pub arrangement: String,
    /// Whether it is an infinite loop by default in that arrangement.
    pub infinite_loop: bool,
    /// The line under the loop switch.
    pub loop_hint: String,
    /// Whether its marker can be deleted: not the first, nor one whose
    /// section is all an arrangement plays.
    pub can_delete: bool,
    /// Why it cannot, when it cannot.
    pub delete_hint: Option<String>,
}

/// A tab of the "Arrangement" panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrangementTab {
    /// Its arrangement, counted from 0 in package order; selects it.
    pub index: usize,
    /// Its name.
    pub name: String,
    /// Whether it is the arrangement shown.
    pub active: bool,
    /// The name being typed, while it is renamed.
    pub renaming: Option<FieldView>,
}

/// A position of the arrangement shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipView {
    /// Its position, counted from 0.
    pub index: usize,
    /// Its section's label, such as "Chorus 1".
    pub label: String,
    /// Its section's abbreviation.
    pub abbr: String,
    /// Its section's icon.
    pub icon: &'static str,
    /// Its section's family.
    pub kind: SectionKind,
    /// Its section's bars, such as "16".
    pub bars: String,
    /// Whether it loops by default in this arrangement.
    pub looped: bool,
}

/// A button of "Add a section to the end".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteButton {
    /// The section's marker, counted from 0.
    pub index: usize,
    /// Its label.
    pub label: String,
    /// Its icon.
    pub icon: &'static str,
    /// Its family.
    pub kind: SectionKind,
}

/// The "Arrangement" panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrangementView {
    /// Every arrangement, by id, in package order.
    pub tabs: Vec<ArrangementTab>,
    /// Whether the arrangement shown can be renamed and deleted: every one
    /// but Default.
    pub editable: bool,
    /// Its positions, in play order.
    pub chips: Vec<ChipView>,
    /// Where a dragged chip would land: before the chip at this index, or
    /// after the last one when it equals the chip count.
    pub insertion: Option<usize>,
    /// One button per section, in song order.
    pub palette: Vec<PaletteButton>,
    /// "11 sections · 136 bars · 5:14" ([`domain::ArrangementSummary`]).
    pub summary: String,
    /// The sections of the summary, such as "11".
    pub sections: String,
    /// Its bars, such as "136".
    pub bars: String,
    /// Its length, such as "5:14".
    pub time: String,
}

/// What the Song editor shows for a song of the library.
#[derive(Debug, Clone, PartialEq)]
pub struct SongEditView {
    /// The header, the notes and the stems, as the import's editor shows
    /// them; its refusals are the note ([`SongEditorView::save_problem`]).
    pub editor: SongEditorView,
    /// Whether Tempo, Time and the changes are read-only: the song's tempo
    /// or meter changes do not start bars.
    pub maps_read_only: bool,
    /// The "Section markers" panel.
    pub markers: MarkersView,
    /// The marker inspector.
    pub inspector: InspectorView,
    /// The "Arrangement" panel.
    pub arrangement: ArrangementView,
}

#[cfg(test)]
mod tests {
    use domain::{
        Arrangement, BarBeat, SectionMarker, SectionType, SongEditor, SongPackage, Stem, StemRole,
        TempoPoint, TimeSignaturePoint,
    };

    use super::{
        ALONG, BarScale, WAVE_LOW, Window, bar_label, bars_text, frame_share, frames_span, ruler,
        wave_bars,
    };
    use crate::view::SectionKind;

    const RATE: u32 = 8_000;

    /// A bar at 120 BPM in 4/4.
    const BAR: u64 = 2 * RATE as u64;

    /// A song of 16 bars: an intro at bar 1 and Verse 1 at bar 5.
    fn editor() -> SongEditor {
        let marker = |id: &str, section_type, number, bar: u64| SectionMarker {
            id: id.to_owned(),
            section_type,
            number,
            start: (bar - 1) * BAR,
        };
        let package = SongPackage {
            format_version: 1,
            title: "Song".to_owned(),
            artist: None,
            sample_rate: RATE,
            key: None,
            tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
            time_signature_map: vec![TimeSignaturePoint {
                at: 0,
                numerator: 4,
                denominator: 4,
            }],
            stems: vec![Stem {
                id: "keys".to_owned(),
                file: "keys.wav".to_owned(),
                part: "keys".to_owned(),
                role: StemRole::Track,
            }],
            sections: vec![
                marker("intro", SectionType::Intro, None, 1),
                marker("verse", SectionType::Verse, Some(1), 5),
            ],
            arrangements: vec![Arrangement {
                id: "default".to_owned(),
                name: "Default".to_owned(),
                sections: vec!["intro".to_owned(), "verse".to_owned()],
                infinite_loop: Vec::new(),
            }],
        };
        SongEditor::new(package, 16 * BAR).unwrap()
    }

    #[test]
    fn the_scale_measures_frames_in_bars_each_one_unit_wide() {
        let editor = editor();
        let scale = BarScale::of(&editor);

        assert_eq!(scale.bars(), 16);
        assert_eq!(scale.bar_range(15), 15 * BAR..16 * BAR);
        assert_eq!(scale.units(0), 0.0);
        assert_eq!(scale.units(2 * BAR + BAR / 4), 2.25);
        assert_eq!(scale.units(16 * BAR), 16.0, "the song end");
        assert_eq!(scale.frames(2.25), 2 * BAR + BAR / 4);
        assert_eq!(scale.frames(-1.0), 0);
        assert_eq!(scale.frames(99.0), 16 * BAR - 1, "inside the song");
    }

    #[test]
    fn the_window_places_frames_across_the_bars_it_shows() {
        let editor = editor();
        let window = Window {
            first: 8,
            visible: 8,
        };

        let span = frames_span(&editor, window, 4 * BAR..12 * BAR).unwrap();
        assert_eq!((span.left, span.width), (0.0, 0.5));
        assert_eq!(frames_span(&editor, window, 0..4 * BAR), None);
        assert_eq!(frame_share(&editor, window, 12 * BAR), Some(0.5));
        assert_eq!(frame_share(&editor, window, 2 * BAR), None);
        assert_eq!(window.units_at(ALONG / 2), 12.0);
        assert_eq!(window.units_at(u32::MAX), 16.0, "at most the right edge");
    }

    #[test]
    fn the_ruler_labels_every_eighth_bar_and_bars_stay_flat_without_an_overview() {
        let editor = editor();
        let all = Window {
            first: 0,
            visible: 16,
        };

        let labels: Vec<u64> = ruler(all).iter().map(|mark| mark.bar).collect();
        assert_eq!(labels, [1, 9]);
        let marks = ruler(Window {
            first: 3,
            visible: 10,
        });
        assert_eq!(marks.len(), 1);
        assert_eq!((marks[0].bar, marks[0].left), (9, 0.5));
        let bars = wave_bars(&editor, None, all);
        assert_eq!(bars.len(), 16);
        assert!(bars.iter().all(|bar| bar.height == WAVE_LOW));
        assert_eq!(bars[3].kind, SectionKind::Neutral);
        assert_eq!(bars[4].kind, SectionKind::Verse, "bar 5 starts the verse");
    }

    #[test]
    fn a_position_reads_as_its_bar_and_a_beat_other_than_the_first() {
        assert_eq!(bar_label(BarBeat { bar: 9, beat: 1 }), "Bar 9");
        assert_eq!(bar_label(BarBeat { bar: 9, beat: 3 }), "Bar 9.3");
        assert_eq!(bars_text(1), "1 bar");
        assert_eq!(bars_text(16), "16 bars");
    }
}
