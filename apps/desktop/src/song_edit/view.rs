//! What the Song editor of a library song shows ([`SongEditView`]), built
//! from its state: the header and the stems as the import's editor shows
//! them, and the sections, the inspector and the arrangement panel
//! ([`crate::song_sections`]).

use domain::{
    Bus, DEFAULT_ARRANGEMENT_ID, Part, SectionType, Snap, SongPackage, StoredFormat, part_for_file,
};

use super::{LOOP_HINT, READ_ONLY_MAPS, SongEdit, Typing, Waveform};
use crate::library_page::Choice;
use crate::mixer::stem_icon;
use crate::song_import::{
    FieldView, PartChoice, PartGroup, PartMenu, SavingView, SongEditorView, SongField, StemRow,
    kilohertz,
};
use crate::song_sections::{
    ArrangementTab, ArrangementView, ChipView, DragGhost, InspectorView, MIN_VISIBLE_BARS,
    MarkerBlock, MarkersView, PaletteButton, SongEditView, WaveformState, bar_label, bars_text,
    frame_share, frames_span, ruler, wave_bars,
};
use crate::view::{SectionKind, section_icon, short_label};

impl SongEdit {
    /// What the Song editor shows.
    pub(crate) fn view(&self) -> SongEditView {
        SongEditView {
            editor: self.header_view(),
            maps_read_only: self.maps_read_only,
            markers: self.markers_view(),
            inspector: self.inspector_view(),
            arrangement: self.arrangement_view(),
        }
    }

    fn header_view(&self) -> SongEditorView {
        let package = self.editor.package();
        let stems = package
            .stems
            .iter()
            .enumerate()
            .map(|(index, stem)| {
                let part = Part::from_id(&stem.part).unwrap_or_else(|| part_for_file(&stem.file));
                StemRow {
                    index,
                    file: stem.file.clone(),
                    icon: stem_icon(stem.role, &stem.part, &stem.id),
                    part,
                    bus: part.bus(),
                    automatic: false,
                    menu_open: self.menu == Some(index),
                }
            })
            .collect::<Vec<_>>();
        let part_menu = self.menu.and_then(|index| {
            let current = stems.get(index)?.part;
            Some(part_menu(index, current))
        });
        let problems = self.problems();
        SongEditorView {
            source: String::new(),
            title: self.header.field(SongField::Title),
            key: self.header.field(SongField::Key),
            tempo: self.header.field(SongField::Tempo),
            time: self.header.field(SongField::Time),
            changes: self.header.change_rows(),
            stems_summary: stems_summary(package),
            stems,
            all_stems: self.all_stems,
            part_menu,
            refused: Vec::new(),
            can_save: problems.is_empty() && self.saving.is_none(),
            problems,
            warnings: if self.maps_read_only {
                vec![READ_ONLY_MAPS.to_owned()]
            } else {
                Vec::new()
            },
            saving: self.saving.map(|_| SavingView {
                label: "Saving…".to_owned(),
                percent: 0,
                cancelling: false,
            }),
            save_problem: self.note.clone(),
        }
    }

    fn markers_view(&self) -> MarkersView {
        let editor = &self.editor;
        let window = self.window();
        let sections = &editor.package().sections;
        let blocks = sections
            .iter()
            .enumerate()
            .filter_map(|(index, marker)| {
                let range = editor.section_range(&marker.id)?;
                let span = frames_span(editor, window, range)?;
                let label = marker.section_type.label(marker.number);
                Some(MarkerBlock {
                    index,
                    abbr: short_label(&label),
                    icon: section_icon(&label),
                    kind: SectionKind::of(&label),
                    starts: bar_label(editor.grid().bar_and_beat(marker.start)),
                    span,
                    selected: index == self.selected,
                    movable: index > 0,
                    label,
                })
            })
            .collect();
        let selection = self
            .selected_marker()
            .and_then(|marker| editor.section_range(&marker.id))
            .and_then(|range| frames_span(editor, window, range));
        let drag = self.marker_drag.and_then(|(index, at)| {
            let at = editor.snap(at, self.snap);
            Some(DragGhost {
                index,
                left: frame_share(editor, window, at)?,
                label: bar_label(editor.grid().bar_and_beat(at)),
            })
        });
        let (overview, waveform) = match &self.waveform {
            Waveform::Ready(overview) => (Some(overview), WaveformState::Ready),
            Waveform::Reading(run) => (
                None,
                WaveformState::Reading {
                    percent: run.percent().unwrap_or(0),
                },
            ),
            Waveform::Failed(problem) => (None, WaveformState::Failed(problem.clone())),
        };
        MarkersView {
            snap_to_bar: self.snap == Snap::Bar,
            can_zoom_in: window.visible > MIN_VISIBLE_BARS.min(self.total_bars()),
            can_zoom_out: window.visible < self.total_bars(),
            first_bar: window.first + 1,
            visible_bars: window.visible,
            total_bars: self.total_bars(),
            ruler: ruler(window),
            blocks,
            bars: wave_bars(editor, overview, window),
            selection,
            cursor: frame_share(editor, window, self.cursor),
            cursor_label: bar_label(editor.grid().bar_and_beat(self.cursor)),
            drag,
            waveform,
        }
    }

    fn inspector_view(&self) -> InspectorView {
        let editor = &self.editor;
        let marker = self.selected_marker();
        let label = marker.map_or_else(String::new, |m| m.section_type.label(m.number));
        let id = marker.map_or("", |m| m.id.as_str());
        let arrangement = self.shown_arrangement();
        let plays = arrangement.is_some_and(|a| a.sections.iter().any(|s| s == id));
        let arrangement_name = arrangement.map_or_else(String::new, |a| a.name.clone());
        InspectorView {
            index: self.selected,
            icon: section_icon(&label),
            kind: SectionKind::of(&label),
            section_type: marker.map_or_else(String::new, |m| m.section_type.name().to_owned()),
            type_menu: self.type_menu.then(|| {
                SectionType::ALL
                    .iter()
                    .map(|&section_type| Choice {
                        label: section_type.name().to_owned(),
                        selected: marker.is_some_and(|m| m.section_type == section_type),
                    })
                    .collect()
            }),
            number: FieldView {
                text: self.number.text().to_owned(),
                editing: self.typing == Some(Typing::Number),
            },
            starts_at: marker.map_or_else(String::new, |m| {
                bar_label(editor.grid().bar_and_beat(m.start))
            }),
            length: editor
                .section_range(id)
                .map_or_else(String::new, |range| bars_text(editor.bars_in(range))),
            infinite_loop: self.looped(id),
            loop_hint: if plays {
                LOOP_HINT.to_owned()
            } else {
                format!("Not in {arrangement_name}")
            },
            arrangement: arrangement_name,
            can_delete: self.selected > 0,
            name: label,
        }
    }

    fn arrangement_view(&self) -> ArrangementView {
        let editor = &self.editor;
        let package = editor.package();
        let tabs = package
            .arrangements
            .iter()
            .enumerate()
            .map(|(index, arrangement)| {
                let active = arrangement.id == self.arrangement;
                ArrangementTab {
                    index,
                    name: arrangement.name.clone(),
                    active,
                    renaming: (active && self.typing == Some(Typing::Rename)).then(|| FieldView {
                        text: self.rename.text().to_owned(),
                        editing: true,
                    }),
                }
            })
            .collect();
        let shown = self.shown_arrangement();
        let section = |id: &str| package.sections.iter().find(|marker| marker.id == id);
        let chips = shown.map_or_else(Vec::new, |arrangement| {
            arrangement
                .sections
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    let label =
                        section(id).map_or_else(|| id.clone(), |m| m.section_type.label(m.number));
                    let bars = editor
                        .section_range(id)
                        .map_or(0, |range| editor.bars_in(range));
                    ChipView {
                        index,
                        abbr: short_label(&label),
                        icon: section_icon(&label),
                        kind: SectionKind::of(&label),
                        bars: bars.to_string(),
                        looped: arrangement.infinite_loop.contains(id),
                        label,
                    }
                })
                .collect()
        });
        let palette = package
            .sections
            .iter()
            .enumerate()
            .map(|(index, marker)| {
                let label = marker.section_type.label(marker.number);
                PaletteButton {
                    index,
                    icon: section_icon(&label),
                    kind: SectionKind::of(&label),
                    label,
                }
            })
            .collect();
        let summary = editor.summary(&self.arrangement);
        ArrangementView {
            tabs,
            editable: self.arrangement != DEFAULT_ARRANGEMENT_ID,
            chips,
            insertion: self.chip_insertion.map(|(_, slot)| slot),
            palette,
            summary: summary.map_or_else(String::new, |summary| summary.to_string()),
            sections: summary.map_or_else(String::new, |s| s.sections.to_string()),
            bars: summary.map_or_else(String::new, |s| s.bars.to_string()),
            time: summary.map_or_else(String::new, |s| {
                let seconds = s.seconds();
                format!("{}:{:02}", seconds / 60, seconds % 60)
            }),
        }
    }
}

/// The part menu of stem `stem`, whose part is `current`: every part,
/// grouped by bus, as the import's editor has it.
fn part_menu(stem: usize, current: Part) -> PartMenu {
    PartMenu {
        stem,
        groups: Bus::ALL
            .iter()
            .map(|&bus| PartGroup {
                bus,
                parts: Part::ALL
                    .iter()
                    .filter(|part| part.bus() == bus)
                    .map(|&part| PartChoice {
                        part,
                        selected: part == current,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// The stems panel's header for a saved song: "16 · 48 kHz · FLAC", the
/// format read from the stems' file names.
pub fn stems_summary(package: &SongPackage) -> String {
    if package.stems.is_empty() {
        return "No stems".to_owned();
    }
    let has = |format: StoredFormat| {
        package.stems.iter().any(|stem| {
            stem.file
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case(format.extension()))
        })
    };
    let format = match (has(StoredFormat::Wav), has(StoredFormat::Flac)) {
        (true, true) => "WAV + FLAC",
        (true, false) => "WAV",
        (false, true) => "FLAC",
        (false, false) => "audio",
    };
    format!(
        "{} · {} · {format}",
        package.stems.len(),
        kilohertz(package.sample_rate)
    )
}
