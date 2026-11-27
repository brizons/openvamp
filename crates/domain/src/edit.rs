//! Editing a song's section markers and arrangements (F2.1–F2.4).
//!
//! A [`SongEditor`] holds a valid [`SongPackage`] and the song's length in
//! frames. Every edit either leaves a package [`crate::validate`] accepts or
//! leaves it unchanged and returns a [`SongEditError`] saying why. Positions
//! are sample offsets, as in the package; markers snap to the bar or beat
//! lines of the song's [`BeatGrid`] ([`Snap`]).
//!
//! The rules of the markers:
//!
//! - the first marker starts the song: it cannot be moved or deleted, and no
//!   two markers share a start;
//! - a marker added inside a section splits it. The new section has the
//!   split section's type and no number, and every arrangement plays it
//!   right after each occurrence of the split section, so every arrangement
//!   still plays the same audio;
//! - a moved marker stays between its neighbours;
//! - a deleted marker's section leaves every arrangement and every
//!   infinite-loop list; the delete is refused if an arrangement would be
//!   left with no sections.
//!
//! Of the arrangements: the Default arrangement cannot be renamed or
//! deleted, every arrangement keeps at least one section, and a section is
//! an infinite loop only in an arrangement that plays it.
//!
//! The editor remembers where each position of each arrangement, as it was
//! opened, went through every edit ([`SongEditor::position_map`]), so a
//! setlist that plays part of an arrangement (F5.3) can be moved to the
//! positions that play the same audio: a split keeps both parts of a
//! position, a moved position keeps its audio, and a removed one (a deleted
//! marker's, or one taken out of the arrangement) plays nowhere. A moved
//! marker changes what its sections play, not which positions play them.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Range, RangeInclusive};

use thiserror::Error;

use crate::grid::{BarBeat, BeatGrid};
use crate::import::{slug_or, unique_id};
use crate::package::{
    Arrangement, DEFAULT_ARRANGEMENT_ID, SectionMarker, SectionType, SongPackage, StemInfo,
    StemRole, TempoPoint, TimeSignaturePoint, ValidationError, validate,
};

/// Which lines a placed or moved marker snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Snap {
    /// The nearest bar line ("Snap to bar" on).
    #[default]
    Bar,
    /// The nearest beat ("Snap to bar" off).
    Beat,
}

/// A song's sections and arrangements as the user edits them; see the
/// [module docs](self).
#[derive(Debug, Clone, PartialEq)]
pub struct SongEditor {
    package: SongPackage,
    length_frames: u64,
    /// Every stem `length_frames` long at the package rate, so
    /// [`crate::validate`] checks edits against the song length.
    infos: Vec<StemInfo>,
    /// The grid of the song's maps, built again when they change.
    grid: BeatGrid,
    /// Per arrangement id, where its positions came from.
    origins: BTreeMap<String, Origins>,
}

/// Where an arrangement's positions came from: the original position each
/// one plays the audio of (a split's new position has its parent's), or
/// `None` for one added since the editor opened.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Origins {
    /// How many positions the arrangement had when the editor opened; 0 for
    /// one made since.
    original: usize,
    /// One per current position.
    now: Vec<Option<usize>>,
}

/// Where an arrangement's positions, as the editor opened them, play now
/// ([`SongEditor::position_map`]). Positions are counted from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionMap {
    /// Per original position, the current positions that play its audio.
    moved: Vec<Vec<usize>>,
    /// How many positions the arrangement has now.
    len: usize,
}

impl PositionMap {
    fn of(origins: &Origins) -> Self {
        let mut moved = vec![Vec::new(); origins.original];
        for (index, origin) in origins.now.iter().enumerate() {
            if let Some(slot) = origin.and_then(|origin| moved.get_mut(origin)) {
                slot.push(index);
            }
        }
        Self {
            moved,
            len: origins.now.len(),
        }
    }

    /// How many positions the arrangement had when the editor opened.
    pub fn original_len(&self) -> usize {
        self.moved.len()
    }

    /// How many positions it has now.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether it has no positions now.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The current positions that play original position `original`, in
    /// order: one, two or more when markers split its section, none when it
    /// was removed (or there was no such position).
    pub fn positions(&self, original: usize) -> &[usize] {
        self.moved.get(original).map_or(&[], Vec::as_slice)
    }

    /// Whether every position plays where it did and nothing was added.
    pub fn is_identity(&self) -> bool {
        self.len == self.moved.len()
            && self
                .moved
                .iter()
                .enumerate()
                .all(|(index, now)| now.as_slice() == [index])
    }

    /// The current positions that play what original positions `first`
    /// to `last` (inclusive) played, in the same order and nothing else: one
    /// run with no gap, every original position still played. `None` when
    /// one of them was removed, when they no longer play in that order or
    /// side by side, or when the range is empty or past the end.
    pub fn range(&self, first: usize, last: usize) -> Option<RangeInclusive<usize>> {
        if first > last || last >= self.moved.len() {
            return None;
        }
        let mut run = self.moved[first..=last].iter().flatten().copied();
        let start = run.next()?;
        let mut end = start;
        for index in run {
            if index != end + 1 {
                return None;
            }
            end = index;
        }
        // Every original position plays somewhere in the run.
        self.moved[first..=last]
            .iter()
            .all(|now| !now.is_empty())
            .then_some(start..=end)
    }
}

/// What an arrangement plays, as the arrangement panel sums it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrangementSummary {
    /// Positions, each repeat counted.
    pub sections: usize,
    /// Bars, counted per position as the bar lines that start inside its
    /// section ([`SongEditor::bars_in`]).
    pub bars: u64,
    /// Frames, each repeat counted.
    pub length_frames: u64,
    /// Frames per second.
    pub sample_rate: u32,
}

impl ArrangementSummary {
    /// The length in whole seconds, rounded down.
    pub fn seconds(&self) -> u64 {
        self.length_frames
            .checked_div(u64::from(self.sample_rate))
            .unwrap_or(0)
    }
}

/// "4 sections · 12 bars · 3:05", as the arrangement panel shows it.
impl fmt::Display for ArrangementSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.seconds();
        write!(
            f,
            "{} · {} · {}:{:02}",
            counted(self.sections as u64, "section"),
            counted(self.bars, "bar"),
            seconds / 60,
            seconds % 60
        )
    }
}

fn counted(count: u64, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}

/// Why an edit was refused; the package is left unchanged. Sections are
/// named by their label ("Verse 1") and arrangements by their name.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SongEditError {
    /// No section has this id.
    #[error("there is no section \"{id}\"")]
    UnknownSection {
        /// The id asked for.
        id: String,
    },
    /// No arrangement has this id.
    #[error("there is no arrangement \"{id}\"")]
    UnknownArrangement {
        /// The id asked for.
        id: String,
    },
    /// A marker already starts where the new one would.
    #[error(
        "\"{label}\" already starts at bar {}, beat {}; two markers cannot share a start",
        .position.bar,
        .position.beat
    )]
    MarkerExists {
        /// Id of the section that starts there.
        section: String,
        /// Its label.
        label: String,
        /// The snapped position.
        at: u64,
        /// Its bar and beat.
        position: BarBeat,
    },
    /// The first marker starts the song.
    #[error("the first marker starts the song; it cannot be moved")]
    FirstMarkerMove,
    /// The first marker starts the song.
    #[error("the first marker starts the song; it cannot be deleted")]
    FirstMarkerDelete,
    /// The move would reach the marker before.
    #[error(
        "\"{label}\" cannot move to or before \"{previous_label}\"; a marker stays between its neighbours"
    )]
    CrossesPrevious {
        /// Id of the moved section.
        section: String,
        /// Its label.
        label: String,
        /// The snapped position.
        at: u64,
        /// Id of the section before it.
        previous: String,
        /// Its label.
        previous_label: String,
    },
    /// The move would reach the marker after.
    #[error(
        "\"{label}\" cannot move to or past \"{next_label}\"; a marker stays between its neighbours"
    )]
    CrossesNext {
        /// Id of the moved section.
        section: String,
        /// Its label.
        label: String,
        /// The snapped position.
        at: u64,
        /// Id of the section after it.
        next: String,
        /// Its label.
        next_label: String,
    },
    /// Deleting the marker would leave an arrangement with no sections.
    #[error("deleting \"{label}\" would leave arrangement \"{name}\" with no sections")]
    EmptiesArrangement {
        /// Id of the section.
        section: String,
        /// Its label.
        label: String,
        /// Id of the arrangement.
        arrangement: String,
        /// Its name.
        name: String,
    },
    /// No stem has this id.
    #[error("there is no stem \"{id}\"")]
    UnknownStem {
        /// The id asked for.
        id: String,
    },
    /// The Default arrangement always exists under its name (F2.3).
    #[error("the Default arrangement cannot be renamed or deleted")]
    DefaultArrangementFixed,
    /// The name is empty or only spaces.
    #[error("an arrangement needs a name")]
    EmptyName,
    /// Another arrangement already has this name, compared without regard
    /// to case.
    #[error("an arrangement named \"{name}\" already exists")]
    NameTaken {
        /// The other arrangement's name, as stored.
        name: String,
    },
    /// No position has this index (for an insert: the index is past the end).
    #[error(
        "position {index} is out of range for arrangement \"{name}\", which has {len} sections"
    )]
    PositionOutOfRange {
        /// Id of the arrangement.
        arrangement: String,
        /// Its name.
        name: String,
        /// The rejected index, counted from 0.
        index: usize,
        /// Number of positions in the arrangement.
        len: usize,
    },
    /// The arrangement's only position cannot be removed.
    #[error("arrangement \"{name}\" needs at least one section")]
    LastPosition {
        /// Id of the arrangement.
        arrangement: String,
        /// Its name.
        name: String,
    },
    /// Only a section the arrangement plays can be its infinite loop.
    #[error("\"{label}\" is not in arrangement \"{name}\"; only its own sections can loop")]
    NotInArrangement {
        /// Id of the section.
        section: String,
        /// Its label.
        label: String,
        /// Id of the arrangement.
        arrangement: String,
        /// Its name.
        name: String,
    },
    /// The edit would break another package rule, such as an arrangement
    /// longer than [`crate::MAX_SONG_HOURS`].
    #[error("{}", joined(.0))]
    Package(Vec<ValidationError>),
}

fn joined(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn label(marker: &SectionMarker) -> String {
    marker.section_type.label(marker.number)
}

impl SongEditor {
    /// An editor for `package`, a song `length_frames` long: the length of
    /// its longest stem. Returns the package's broken rules when it is not
    /// valid for that length.
    pub fn new(package: SongPackage, length_frames: u64) -> Result<Self, Vec<ValidationError>> {
        let infos: Vec<StemInfo> = package
            .stems
            .iter()
            .map(|stem| StemInfo {
                id: stem.id.clone(),
                sample_rate: package.sample_rate,
                length_samples: length_frames,
            })
            .collect();
        let validated = validate(&package, &infos)?;
        let grid = BeatGrid::new(&validated.song);
        let origins = package
            .arrangements
            .iter()
            .map(|arrangement| {
                let len = arrangement.sections.len();
                let origins = Origins {
                    original: len,
                    now: (0..len).map(Some).collect(),
                };
                (arrangement.id.clone(), origins)
            })
            .collect();
        Ok(Self {
            package,
            length_frames,
            infos,
            grid,
            origins,
        })
    }

    /// Where the positions arrangement `id` had when the editor opened play
    /// now (see the [module docs](self)); `None` when it has no such
    /// arrangement now. One made since has no original positions.
    pub fn position_map(&self, id: &str) -> Option<PositionMap> {
        self.arrangement(id)?;
        self.origins.get(id).map(PositionMap::of)
    }

    /// Gives the song new tempo and time-signature maps; the grid follows
    /// and every marker stays on its sample.
    pub fn set_maps(
        &mut self,
        tempo_map: Vec<TempoPoint>,
        time_signature_map: Vec<TimeSignaturePoint>,
    ) -> Result<(), SongEditError> {
        let mut candidate = self.package.clone();
        candidate.tempo_map = tempo_map;
        candidate.time_signature_map = time_signature_map;
        let validated = validate(&candidate, &self.infos).map_err(SongEditError::Package)?;
        self.grid = BeatGrid::new(&validated.song);
        self.package = candidate;
        Ok(())
    }

    /// Gives stem `id` the part `part`, with its `role`.
    pub fn set_stem_part(
        &mut self,
        id: &str,
        part: &str,
        role: StemRole,
    ) -> Result<(), SongEditError> {
        let index = self
            .package
            .stems
            .iter()
            .position(|stem| stem.id == id)
            .ok_or_else(|| SongEditError::UnknownStem { id: id.to_owned() })?;
        let mut candidate = self.package.clone();
        candidate.stems[index].part = part.to_owned();
        candidate.stems[index].role = role;
        self.commit(candidate)
    }

    /// The package as edited so far.
    pub fn package(&self) -> &SongPackage {
        &self.package
    }

    /// The package as edited, to save.
    pub fn into_package(self) -> SongPackage {
        self.package
    }

    /// The song length in frames.
    pub fn length_frames(&self) -> u64 {
        self.length_frames
    }

    /// The song's bar and beat grid.
    pub fn grid(&self) -> &BeatGrid {
        &self.grid
    }

    /// The bar or beat line nearest to `at`; a tie goes to the earlier line.
    /// Lines start before the song end, so a position at or past it snaps to
    /// the last one.
    pub fn snap(&self, at: u64, snap: Snap) -> u64 {
        let lines = match snap {
            Snap::Bar => self.grid.bar_lines(),
            Snap::Beat => self.grid.beat_lines(),
        };
        let after = lines.partition_point(|&line| line <= at);
        let before = after.checked_sub(1).map(|index| lines[index]);
        match (before, lines.get(after).copied()) {
            (Some(before), Some(after)) if after - at < at - before => after,
            (Some(before), _) => before,
            (None, after) => after.unwrap_or(0),
        }
    }

    /// The samples section `id` plays: from its marker to the next one or
    /// the song end.
    pub fn section_range(&self, id: &str) -> Option<Range<u64>> {
        self.marker_index(id).map(|index| self.range_at(index))
    }

    /// The bars that start inside `range`: a section on the bar grid counts
    /// its bars, and a bar cut by the song end counts where it starts.
    pub fn bars_in(&self, range: Range<u64>) -> u64 {
        let lines = self.grid.bar_lines();
        let first = lines.partition_point(|&line| line < range.start);
        let end = lines.partition_point(|&line| line < range.end);
        end.saturating_sub(first) as u64
    }

    /// The summary of arrangement `id`, or `None` when there is none.
    pub fn summary(&self, id: &str) -> Option<ArrangementSummary> {
        let arrangement = self.arrangement(id)?;
        let (mut bars, mut length_frames) = (0, 0);
        for range in arrangement
            .sections
            .iter()
            .filter_map(|section| self.section_range(section))
        {
            bars += self.bars_in(range.clone());
            length_frames += range.end - range.start;
        }
        Some(ArrangementSummary {
            sections: arrangement.sections.len(),
            bars,
            length_frames,
            sample_rate: self.package.sample_rate,
        })
    }

    /// Adds a marker at `at`, snapped, splitting the section it falls in
    /// (F2.1); returns the new section's id.
    pub fn add_marker(&mut self, at: u64, snap: Snap) -> Result<String, SongEditError> {
        let start = self.snap(at, snap);
        let sections = &self.package.sections;
        let split = sections
            .partition_point(|marker| marker.start <= start)
            .saturating_sub(1);
        let split_marker = &sections[split];
        if split_marker.start == start {
            return Err(SongEditError::MarkerExists {
                section: split_marker.id.clone(),
                label: label(split_marker),
                at: start,
                position: self.grid.bar_and_beat(start),
            });
        }
        let base = slug_or(split_marker.section_type.name(), "section");
        let id = unique_id(&base, |id| sections.iter().any(|marker| marker.id == id));
        let split_id = split_marker.id.clone();
        let marker = SectionMarker {
            id: id.clone(),
            section_type: split_marker.section_type,
            number: None,
            start,
        };

        let mut candidate = self.package.clone();
        let mut origins = self.origins.clone();
        candidate.sections.insert(split + 1, marker);
        for arrangement in &mut candidate.arrangements {
            let tracked = origins.get_mut(&arrangement.id);
            let old = tracked.as_ref().map(|t| t.now.clone()).unwrap_or_default();
            let mut sections = Vec::with_capacity(arrangement.sections.len() + 1);
            let mut now = Vec::with_capacity(old.len() + 1);
            for (index, section) in arrangement.sections.drain(..).enumerate() {
                let origin = old.get(index).copied().flatten();
                let splits = section == split_id;
                sections.push(section);
                now.push(origin);
                if splits {
                    // The new part plays the rest of its parent's audio.
                    sections.push(id.clone());
                    now.push(origin);
                }
            }
            arrangement.sections = sections;
            if let Some(tracked) = tracked {
                tracked.now = now;
            }
        }
        self.commit_tracked(candidate, origins)?;
        Ok(id)
    }

    /// Moves the marker of section `id` to `at`, snapped (F2.1); returns
    /// its new start. Every arrangement that plays the section follows.
    pub fn move_marker(&mut self, id: &str, at: u64, snap: Snap) -> Result<u64, SongEditError> {
        let index = self.find_marker(id)?;
        if index == 0 {
            return Err(SongEditError::FirstMarkerMove);
        }
        let start = self.snap(at, snap);
        let sections = &self.package.sections;
        let marker = &sections[index];
        let previous = &sections[index - 1];
        if start <= previous.start {
            return Err(SongEditError::CrossesPrevious {
                section: marker.id.clone(),
                label: label(marker),
                at: start,
                previous: previous.id.clone(),
                previous_label: label(previous),
            });
        }
        if let Some(next) = sections.get(index + 1)
            && start >= next.start
        {
            return Err(SongEditError::CrossesNext {
                section: marker.id.clone(),
                label: label(marker),
                at: start,
                next: next.id.clone(),
                next_label: label(next),
            });
        }
        let mut candidate = self.package.clone();
        candidate.sections[index].start = start;
        self.commit(candidate)?;
        Ok(start)
    }

    /// Gives section `id` a type from the built-in list and an optional
    /// number (F2.2).
    pub fn set_marker_type(
        &mut self,
        id: &str,
        section_type: SectionType,
        number: Option<u32>,
    ) -> Result<(), SongEditError> {
        let index = self.find_marker(id)?;
        let mut candidate = self.package.clone();
        let marker = &mut candidate.sections[index];
        marker.section_type = section_type;
        marker.number = number;
        self.commit(candidate)
    }

    /// Deletes the marker of section `id` (F2.1): the section before it
    /// runs on to the next marker, and the section leaves every arrangement
    /// and every infinite-loop list.
    pub fn delete_marker(&mut self, id: &str) -> Result<(), SongEditError> {
        let index = self.find_marker(id)?;
        if index == 0 {
            return Err(SongEditError::FirstMarkerDelete);
        }
        if let Some(arrangement) = self
            .package
            .arrangements
            .iter()
            .find(|arrangement| arrangement.sections.iter().all(|section| section == id))
        {
            return Err(SongEditError::EmptiesArrangement {
                section: id.to_owned(),
                label: label(&self.package.sections[index]),
                arrangement: arrangement.id.clone(),
                name: arrangement.name.clone(),
            });
        }
        let mut candidate = self.package.clone();
        let mut origins = self.origins.clone();
        candidate.sections.remove(index);
        for arrangement in &mut candidate.arrangements {
            if let Some(tracked) = origins.get_mut(&arrangement.id) {
                let mut kept = arrangement.sections.iter().map(|section| section != id);
                tracked.now.retain(|_| kept.next().unwrap_or(true));
            }
            arrangement.sections.retain(|section| section != id);
            arrangement.infinite_loop.retain(|section| section != id);
        }
        self.commit_tracked(candidate, origins)
    }

    /// Creates an arrangement named `name` that plays every section once,
    /// in song order (F2.3); returns its id, a unique slug of the name.
    pub fn new_arrangement(&mut self, name: &str) -> Result<String, SongEditError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(SongEditError::EmptyName);
        }
        self.check_name_free(name, None)?;
        let arrangements = &self.package.arrangements;
        let id = unique_id(&slug_or(name, "arrangement"), |id| {
            arrangements.iter().any(|arrangement| arrangement.id == id)
        });
        let mut candidate = self.package.clone();
        let sections: Vec<String> = self
            .package
            .sections
            .iter()
            .map(|marker| marker.id.clone())
            .collect();
        let mut origins = self.origins.clone();
        origins.insert(
            id.clone(),
            Origins {
                original: 0,
                now: vec![None; sections.len()],
            },
        );
        candidate.arrangements.push(Arrangement {
            id: id.clone(),
            name: name.to_owned(),
            sections,
            infinite_loop: Vec::new(),
        });
        self.commit_tracked(candidate, origins)?;
        Ok(id)
    }

    /// Renames arrangement `id`; its id stays.
    pub fn rename_arrangement(&mut self, id: &str, name: &str) -> Result<(), SongEditError> {
        let index = self.find_arrangement(id)?;
        if id == DEFAULT_ARRANGEMENT_ID {
            return Err(SongEditError::DefaultArrangementFixed);
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(SongEditError::EmptyName);
        }
        self.check_name_free(name, Some(index))?;
        let mut candidate = self.package.clone();
        candidate.arrangements[index].name = name.to_owned();
        self.commit(candidate)
    }

    /// Refuses `name` when an arrangement other than the one at `except`
    /// already has it, compared without regard to case, so tabs and lists
    /// never show two arrangements under one name.
    fn check_name_free(&self, name: &str, except: Option<usize>) -> Result<(), SongEditError> {
        let taken = self
            .package
            .arrangements
            .iter()
            .enumerate()
            .find(|(index, other)| {
                Some(*index) != except && other.name.trim().to_lowercase() == name.to_lowercase()
            });
        match taken {
            Some((_, other)) => Err(SongEditError::NameTaken {
                name: other.name.clone(),
            }),
            None => Ok(()),
        }
    }

    /// Deletes arrangement `id`; the Default arrangement cannot be deleted
    /// (F2.3).
    pub fn delete_arrangement(&mut self, id: &str) -> Result<(), SongEditError> {
        let index = self.find_arrangement(id)?;
        if id == DEFAULT_ARRANGEMENT_ID {
            return Err(SongEditError::DefaultArrangementFixed);
        }
        let mut candidate = self.package.clone();
        candidate.arrangements.remove(index);
        let mut origins = self.origins.clone();
        origins.remove(id);
        self.commit_tracked(candidate, origins)
    }

    /// Inserts section `section` at position `index` of arrangement
    /// `arrangement`, from 0 to its number of positions (the end); a
    /// section may repeat (F2.3).
    pub fn insert_section(
        &mut self,
        arrangement: &str,
        index: usize,
        section: &str,
    ) -> Result<(), SongEditError> {
        let at = self.find_arrangement(arrangement)?;
        self.find_marker(section)?;
        let len = self.package.arrangements[at].sections.len();
        if index > len {
            return Err(self.out_of_range(at, index));
        }
        let mut candidate = self.package.clone();
        candidate.arrangements[at]
            .sections
            .insert(index, section.to_owned());
        let origins = self.track(arrangement, |now| now.insert(index, None));
        self.commit_tracked(candidate, origins)
    }

    /// Adds section `section` at the end of arrangement `arrangement`.
    pub fn append_section(
        &mut self,
        arrangement: &str,
        section: &str,
    ) -> Result<(), SongEditError> {
        let at = self.find_arrangement(arrangement)?;
        let len = self.package.arrangements[at].sections.len();
        self.insert_section(arrangement, len, section)
    }

    /// Removes and returns the section at position `index` of arrangement
    /// `arrangement`. A section whose last occurrence goes stops being an
    /// infinite loop there.
    pub fn remove_section(
        &mut self,
        arrangement: &str,
        index: usize,
    ) -> Result<String, SongEditError> {
        let at = self.find_arrangement(arrangement)?;
        self.check_position(at, index)?;
        let current = &self.package.arrangements[at];
        if current.sections.len() == 1 {
            return Err(SongEditError::LastPosition {
                arrangement: current.id.clone(),
                name: current.name.clone(),
            });
        }
        let mut candidate = self.package.clone();
        let target = &mut candidate.arrangements[at];
        let removed = target.sections.remove(index);
        if !target.sections.contains(&removed) {
            target.infinite_loop.retain(|section| *section != removed);
        }
        let origins = self.track(arrangement, |now| {
            now.remove(index);
        });
        self.commit_tracked(candidate, origins)?;
        Ok(removed)
    }

    /// Moves the section at position `from` of arrangement `arrangement` so
    /// it ends up at position `to`, shifting the ones in between (F2.3).
    pub fn move_section(
        &mut self,
        arrangement: &str,
        from: usize,
        to: usize,
    ) -> Result<(), SongEditError> {
        let at = self.find_arrangement(arrangement)?;
        self.check_position(at, from)?;
        self.check_position(at, to)?;
        let mut candidate = self.package.clone();
        let sections = &mut candidate.arrangements[at].sections;
        let section = sections.remove(from);
        sections.insert(to, section);
        let origins = self.track(arrangement, |now| {
            let origin = now.remove(from);
            now.insert(to, origin);
        });
        self.commit_tracked(candidate, origins)
    }

    /// Marks section `section` as an infinite loop by default in
    /// arrangement `arrangement`, or clears the mark (F2.4). The
    /// arrangement must play the section.
    pub fn set_infinite_loop(
        &mut self,
        arrangement: &str,
        section: &str,
        infinite_loop: bool,
    ) -> Result<(), SongEditError> {
        let at = self.find_arrangement(arrangement)?;
        let marker = self.find_marker(section)?;
        let current = &self.package.arrangements[at];
        if !current.sections.iter().any(|id| id == section) {
            return Err(SongEditError::NotInArrangement {
                section: section.to_owned(),
                label: label(&self.package.sections[marker]),
                arrangement: current.id.clone(),
                name: current.name.clone(),
            });
        }
        let mut candidate = self.package.clone();
        let looped = &mut candidate.arrangements[at].infinite_loop;
        if !infinite_loop {
            looped.retain(|id| id != section);
        } else if !looped.iter().any(|id| id == section) {
            looped.push(section.to_owned());
        }
        self.commit(candidate)
    }

    /// Keeps `candidate` if [`crate::validate`] accepts it; otherwise the
    /// package stays as it was.
    fn commit(&mut self, candidate: SongPackage) -> Result<(), SongEditError> {
        let origins = self.origins.clone();
        self.commit_tracked(candidate, origins)
    }

    /// [`Self::commit`], keeping `origins` as where the positions came from
    /// when `candidate` is kept.
    fn commit_tracked(
        &mut self,
        candidate: SongPackage,
        origins: BTreeMap<String, Origins>,
    ) -> Result<(), SongEditError> {
        validate(&candidate, &self.infos).map_err(SongEditError::Package)?;
        self.package = candidate;
        self.origins = origins;
        Ok(())
    }

    /// Where the positions came from, with `change` made to those of
    /// arrangement `id`, as its sections change.
    fn track(
        &self,
        id: &str,
        change: impl FnOnce(&mut Vec<Option<usize>>),
    ) -> BTreeMap<String, Origins> {
        let mut origins = self.origins.clone();
        if let Some(tracked) = origins.get_mut(id) {
            change(&mut tracked.now);
        }
        origins
    }

    fn marker_index(&self, id: &str) -> Option<usize> {
        self.package
            .sections
            .iter()
            .position(|marker| marker.id == id)
    }

    fn find_marker(&self, id: &str) -> Result<usize, SongEditError> {
        self.marker_index(id)
            .ok_or_else(|| SongEditError::UnknownSection { id: id.to_owned() })
    }

    fn range_at(&self, index: usize) -> Range<u64> {
        let sections = &self.package.sections;
        let end = sections
            .get(index + 1)
            .map_or(self.length_frames, |next| next.start);
        sections[index].start..end
    }

    fn arrangement(&self, id: &str) -> Option<&Arrangement> {
        self.package
            .arrangements
            .iter()
            .find(|arrangement| arrangement.id == id)
    }

    fn find_arrangement(&self, id: &str) -> Result<usize, SongEditError> {
        self.package
            .arrangements
            .iter()
            .position(|arrangement| arrangement.id == id)
            .ok_or_else(|| SongEditError::UnknownArrangement { id: id.to_owned() })
    }

    fn check_position(&self, arrangement: usize, index: usize) -> Result<(), SongEditError> {
        if index < self.package.arrangements[arrangement].sections.len() {
            Ok(())
        } else {
            Err(self.out_of_range(arrangement, index))
        }
    }

    fn out_of_range(&self, arrangement: usize, index: usize) -> SongEditError {
        let arrangement = &self.package.arrangements[arrangement];
        SongEditError::PositionOutOfRange {
            arrangement: arrangement.id.clone(),
            name: arrangement.name.clone(),
            index,
            len: arrangement.sections.len(),
        }
    }
}
