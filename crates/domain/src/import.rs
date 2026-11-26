//! The song an import is about to save (F1.1–F1.5), before it is a package.
//!
//! The library layer reads the stems of a folder or ZIP and gives a
//! [`SongDraft`]: the title, the tempo, the time signature and the key the
//! user can edit, and one [`DraftStem`] per audio file, with the facts read
//! from it and the [`Part`] its file name gives it. [`SongDraft::to_package`]
//! turns the draft into a [`SongPackage`] and checks it with
//! [`crate::validate`], so the package an import saves is one the player
//! loads.
//!
//! The package it builds:
//!
//! - stem ids are slugs of the file names (`EG 1.wav` is `eg-1`), unique in
//!   the song (`eg-1-2` for a second one), and each stem is stored as
//!   `stems/<id>.wav` or `stems/<id>.flac` ([`StoredFormat`]);
//! - each stem's `part` is its part's id and its role is [`Part::role`];
//! - the sample rate is the one most stems share; a stem at another rate is a
//!   [`DraftError::SampleRateMismatch`] that names its file;
//! - the tempo and time-signature maps start with the draft's tempo and time
//!   signature at 0; each [`MapChange`] is entered at a bar number, bar 1
//!   being the start, and stored at the sample where that bar starts on the
//!   grid of the changes before it (the [`BeatGrid`] the player uses), so
//!   bars restart at a time-signature change as they do everywhere else;
//! - one Intro section at 0 ([`INTRO_SECTION_ID`]) and the Default
//!   arrangement ([`DEFAULT_ARRANGEMENT_ID`]) holding it: marking the real
//!   sections is F2.

use std::collections::{HashMap, HashSet};
use std::fmt;

use thiserror::Error;

use crate::bus::{file_name, without_accent};
use crate::compile::ClickLevel;
use crate::grid::BeatGrid;
use crate::package::{
    Arrangement, DEFAULT_ARRANGEMENT_ID, FORMAT_VERSION, MAX_BPM, MAX_SONG_HOURS, MIN_BPM,
    SectionMarker, SectionType, SongPackage, Stem, StemInfo, TempoPoint, TimeSignaturePoint,
    ValidationError, ValidationWarning, bpm_in_range, denominator_valid, numerator_valid, validate,
};
use crate::part::{Part, part_for_file};
use crate::setlist::TimeSignature;
use crate::song::Song;

/// Tempo of a new draft, in beats per minute.
pub const DEFAULT_IMPORT_BPM: f64 = 120.0;

/// Time signature of a new draft: 4/4.
pub const DEFAULT_IMPORT_TIME_SIGNATURE: TimeSignature = TimeSignature {
    numerator: 4,
    denominator: 4,
};

/// Id of the one section an imported song starts with.
pub const INTRO_SECTION_ID: &str = "intro";

/// Name of the Default arrangement an imported song gets.
pub const DEFAULT_ARRANGEMENT_NAME: &str = "Default";

/// Folder of the song folder that holds the imported stems.
pub const STEMS_FOLDER: &str = "stems";

/// Longest stem id, in bytes, before a number that makes it unique.
const MAX_ID_BYTES: usize = 64;

/// The file format a stem is stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoredFormat {
    /// A WAV file, copied as it was.
    Wav,
    /// A FLAC file, copied as it was or written by the import.
    Flac,
}

impl StoredFormat {
    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
}

/// One stem of a [`SongDraft`]: an audio file of the source, with what the
/// import read from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStem {
    /// The file's path inside the source, with `/` between folders, such as
    /// `Guitars/EG 1.wav`: what the user recognizes and errors name.
    pub source_name: String,
    /// How the stem is stored in the song folder.
    pub stored_as: StoredFormat,
    /// Frames per second.
    pub sample_rate: u32,
    /// Length in frames, as the stored file will hold it.
    pub length_frames: u64,
    /// The stem's part.
    pub part: Part,
    /// Whether the import chose the part from the file name; `false` once
    /// the user picked one ([`SongDraft::set_part`]).
    pub automatic: bool,
}

impl DraftStem {
    /// A stem with the part its file name gives it ([`part_for_file`]),
    /// marked automatic.
    pub fn new(
        source_name: impl Into<String>,
        stored_as: StoredFormat,
        sample_rate: u32,
        length_frames: u64,
    ) -> Self {
        let source_name = source_name.into();
        Self {
            part: part_for_file(&source_name),
            automatic: true,
            source_name,
            stored_as,
            sample_rate,
            length_frames,
        }
    }
}

/// A tempo change, a time-signature change, or both, at the start of a bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapChange {
    /// The bar the change starts, counted from 1 (the start of the song);
    /// a change can start bar 2 or later.
    pub bar: u64,
    /// The new tempo, in beats per minute, if the change sets one.
    pub bpm: Option<f64>,
    /// The new time signature, if the change sets one; bars restart there.
    pub time_signature: Option<TimeSignature>,
}

/// The song an import will save, as the user edits it (F1.3).
#[derive(Debug, Clone, PartialEq)]
pub struct SongDraft {
    /// Song title; it must not be empty.
    pub title: String,
    /// Musical key, optional free text.
    pub key: Option<String>,
    /// Tempo at the start, in beats per minute counting the time-signature
    /// denominator note.
    pub bpm: f64,
    /// Time signature at the start.
    pub time_signature: TimeSignature,
    /// Tempo and time-signature changes, in any order.
    pub changes: Vec<MapChange>,
    stems: Vec<DraftStem>,
}

/// A draft that passed every check, as the package to save.
#[derive(Debug, Clone, PartialEq)]
pub struct PackagedDraft {
    /// The manifest to write as `song.json`.
    pub package: SongPackage,
    /// The validated song the package is.
    pub song: Song,
    /// Findings that do not block saving.
    pub warnings: Vec<DraftWarning>,
}

/// Why a draft cannot be saved; [`SongDraft::to_package`] reports all of
/// them at once.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DraftError {
    /// The title is empty or only spaces.
    #[error("the song needs a title")]
    EmptyTitle,
    /// The draft has no stems.
    #[error("the import has no stems")]
    NoStems,
    /// A tempo is not a number from [`MIN_BPM`] to [`MAX_BPM`].
    #[error("the tempo at bar {bar} is {bpm} BPM; it must be from {MIN_BPM} to {MAX_BPM} BPM")]
    BpmOutOfRange {
        /// The bar it starts at: 1 for the song's own tempo.
        bar: u64,
        /// The tempo entered.
        bpm: f64,
    },
    /// A time signature has no beats or a note value other than 1, 2, 4, 8,
    /// 16 or 32.
    #[error(
        "the time signature at bar {bar} is {numerator}/{denominator}; it needs at least 1 beat per bar and a note value of 1, 2, 4, 8, 16 or 32"
    )]
    InvalidTimeSignature {
        /// The bar it starts at: 1 for the song's own time signature.
        bar: u64,
        /// Beats per bar entered.
        numerator: u32,
        /// Note value entered.
        denominator: u32,
    },
    /// A change is at bar 0; bars count from 1.
    #[error("a change is at bar 0; bars count from 1")]
    ChangeAtBarZero,
    /// A change is at bar 1, the start of the song, whose own tempo and time
    /// signature are edited instead.
    #[error(
        "a change is at bar 1, the start of the song; set the song's tempo and time signature instead"
    )]
    ChangeAtBarOne,
    /// Two changes start the same bar.
    #[error("two changes are at bar {bar}; merge them into one")]
    DuplicateChange {
        /// The bar.
        bar: u64,
    },
    /// A change sets neither a tempo nor a time signature.
    #[error("the change at bar {bar} sets neither a tempo nor a time signature")]
    EmptyChange {
        /// The bar.
        bar: u64,
    },
    /// A change starts a bar the song does not reach.
    #[error(
        "the change at bar {bar} is past the end of the song, whose last bar is bar {last_bar}"
    )]
    ChangePastEnd {
        /// The bar.
        bar: u64,
        /// The last bar that starts before the song end.
        last_bar: u64,
    },
    /// A stem's sample rate is not the song's (F1.5).
    #[error(
        "\"{file}\" is {found} Hz, but the song is {expected} Hz; every stem must have the same sample rate"
    )]
    SampleRateMismatch {
        /// The stem's file in the source.
        file: String,
        /// Its sample rate.
        found: u32,
        /// The rate most stems share.
        expected: u32,
    },
    /// Another package rule ([`crate::validate`]), with the files of the
    /// stems it is about.
    #[error("{error}{}", files_suffix(files))]
    Package {
        /// The rule.
        error: ValidationError,
        /// Files of the stems it names, in the source.
        files: Vec<String>,
    },
}

/// A finding that does not block saving.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DraftWarning {
    /// Stem lengths differ by more than one second (F1.5).
    #[error(
        "stem lengths differ by more than one second: \"{shortest}\" has {shortest_length} frames, \"{longest}\" has {longest_length}"
    )]
    StemLengthsDiffer {
        /// File of the shortest stem.
        shortest: String,
        /// Its length in frames.
        shortest_length: u64,
        /// File of the longest stem.
        longest: String,
        /// Its length in frames.
        longest_length: u64,
    },
}

impl SongDraft {
    /// A draft with `stems`, titled `title`, at [`DEFAULT_IMPORT_BPM`] in
    /// [`DEFAULT_IMPORT_TIME_SIGNATURE`], with no key and no changes.
    pub fn new(title: impl Into<String>, stems: Vec<DraftStem>) -> Self {
        Self {
            title: title.into(),
            key: None,
            bpm: DEFAULT_IMPORT_BPM,
            time_signature: DEFAULT_IMPORT_TIME_SIGNATURE,
            changes: Vec::new(),
            stems,
        }
    }

    /// The stems, in source order.
    pub fn stems(&self) -> &[DraftStem] {
        &self.stems
    }

    /// Gives stem `index` the part `part`, chosen by the user, so it is no
    /// longer automatic. Returns `false` when there is no such stem.
    pub fn set_part(&mut self, index: usize, part: Part) -> bool {
        let Some(stem) = self.stems.get_mut(index) else {
            return false;
        };
        stem.part = part;
        stem.automatic = false;
        true
    }

    /// The song's sample rate: the rate most stems share, the first stem's
    /// among equals; `None` without stems.
    pub fn sample_rate(&self) -> Option<u32> {
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for stem in &self.stems {
            *counts.entry(stem.sample_rate).or_default() += 1;
        }
        let most = counts.values().copied().max()?;
        self.stems
            .iter()
            .map(|stem| stem.sample_rate)
            .find(|rate| counts[rate] == most)
    }

    /// The song length in frames: the longest stem's.
    pub fn length_frames(&self) -> u64 {
        self.stems
            .iter()
            .map(|stem| stem.length_frames)
            .max()
            .unwrap_or(0)
    }

    /// The id each stem gets, in stem order: see the [module docs](self).
    pub fn stem_ids(&self) -> Vec<String> {
        let mut taken = HashSet::new();
        self.stems
            .iter()
            .map(|stem| {
                let base = slug(file_name(&stem.source_name));
                let id = (1u64..)
                    .map(|n| match n {
                        1 => base.clone(),
                        n => format!("{base}-{n}"),
                    })
                    .find(|id| !taken.contains(id))
                    .expect("some numbered id is free");
                taken.insert(id.clone());
                id
            })
            .collect()
    }

    /// The package this draft saves as, checked with [`crate::validate`]
    /// against the stems' facts; every problem is returned at once.
    pub fn to_package(&self) -> Result<PackagedDraft, Vec<DraftError>> {
        let mut errors = Vec::new();
        let title = self.title.trim();
        if title.is_empty() {
            errors.push(DraftError::EmptyTitle);
        }
        let Some(sample_rate) = self.sample_rate() else {
            errors.push(DraftError::NoStems);
            errors.extend(self.check_values());
            return Err(errors);
        };
        let value_errors = self.check_values();
        let maps = if value_errors.is_empty() {
            self.maps(sample_rate, &mut errors)
        } else {
            errors.extend(value_errors);
            None
        };

        let ids = self.stem_ids();
        let stems: Vec<Stem> = self
            .stems
            .iter()
            .zip(&ids)
            .map(|(stem, id)| Stem {
                id: id.clone(),
                file: format!("{STEMS_FOLDER}/{id}.{}", stem.stored_as.extension()),
                part: stem.part.id().to_owned(),
                role: stem.part.role(),
            })
            .collect();
        let infos: Vec<StemInfo> = self
            .stems
            .iter()
            .zip(&ids)
            .map(|(stem, id)| StemInfo {
                id: id.clone(),
                sample_rate: stem.sample_rate,
                length_samples: stem.length_frames,
            })
            .collect();
        let (tempo_map, time_signature_map) = maps.unwrap_or_default();
        let package = SongPackage {
            format_version: FORMAT_VERSION,
            title: title.to_owned(),
            artist: None,
            sample_rate,
            key: self
                .key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_owned),
            tempo_map,
            time_signature_map,
            stems,
            sections: vec![SectionMarker {
                id: INTRO_SECTION_ID.to_owned(),
                section_type: SectionType::Intro,
                number: None,
                start: 0,
            }],
            arrangements: vec![Arrangement {
                id: DEFAULT_ARRANGEMENT_ID.to_owned(),
                name: DEFAULT_ARRANGEMENT_NAME.to_owned(),
                sections: vec![INTRO_SECTION_ID.to_owned()],
                infinite_loop: Vec::new(),
            }],
        };
        if !errors.is_empty() {
            // The maps were not built, or the draft is already refused:
            // only the stem rules can add anything.
            errors.extend(self.stem_errors(&package, &infos, &ids));
            return Err(errors);
        }
        match validate(&package, &infos) {
            Ok(validated) => Ok(PackagedDraft {
                warnings: validated
                    .warnings
                    .iter()
                    .map(|warning| self.warning(warning, &ids))
                    .collect(),
                song: validated.song,
                package,
            }),
            Err(rules) => Err(rules
                .into_iter()
                .map(|error| self.error(error, &ids))
                .collect()),
        }
    }

    /// The tempo and time-signature values the user entered that break a
    /// rule, and the changes that are not at a bar after the first.
    fn check_values(&self) -> Vec<DraftError> {
        let mut errors = Vec::new();
        check_bpm(1, self.bpm, &mut errors);
        check_time_signature(1, self.time_signature, &mut errors);
        let mut bars = HashSet::new();
        let mut repeated = HashSet::new();
        for change in self.sorted_changes() {
            match change.bar {
                0 => errors.push(DraftError::ChangeAtBarZero),
                1 => errors.push(DraftError::ChangeAtBarOne),
                bar if !bars.insert(bar) => {
                    if repeated.insert(bar) {
                        errors.push(DraftError::DuplicateChange { bar });
                    }
                }
                _ => {}
            }
            if change.bpm.is_none() && change.time_signature.is_none() {
                errors.push(DraftError::EmptyChange { bar: change.bar });
            }
            if let Some(bpm) = change.bpm {
                check_bpm(change.bar, bpm, &mut errors);
            }
            if let Some(time_signature) = change.time_signature {
                check_time_signature(change.bar, time_signature, &mut errors);
            }
        }
        errors
    }

    /// The changes sorted by bar.
    fn sorted_changes(&self) -> Vec<MapChange> {
        let mut changes = self.changes.clone();
        changes.sort_by_key(|change| change.bar);
        changes
    }

    /// The tempo and time-signature maps, every change placed on the grid of
    /// the ones before it; `None` when a change does not fit in the song
    /// (each such change is in `errors`). The values must have passed
    /// [`Self::check_values`].
    fn maps(
        &self,
        sample_rate: u32,
        errors: &mut Vec<DraftError>,
    ) -> Option<(Vec<TempoPoint>, Vec<TimeSignaturePoint>)> {
        let mut tempo_map = vec![TempoPoint {
            at: 0,
            bpm: self.bpm,
        }];
        let mut time_signature_map = vec![TimeSignaturePoint {
            at: 0,
            numerator: self.time_signature.numerator,
            denominator: self.time_signature.denominator,
        }];
        let length = self.length_frames();
        if length > MAX_SONG_HOURS * 3_600 * u64::from(sample_rate) {
            // `validate` refuses the length; the grid is not built.
            return Some((tempo_map, time_signature_map));
        }
        let mut fits = true;
        for change in self.sorted_changes() {
            let bars = bar_lines(sample_rate, length, &tempo_map, &time_signature_map);
            let Some(&at) = usize::try_from(change.bar - 1)
                .ok()
                .and_then(|index| bars.get(index))
            else {
                errors.push(DraftError::ChangePastEnd {
                    bar: change.bar,
                    last_bar: bars.len() as u64,
                });
                fits = false;
                continue;
            };
            if let Some(bpm) = change.bpm {
                tempo_map.push(TempoPoint { at, bpm });
            }
            if let Some(time_signature) = change.time_signature {
                time_signature_map.push(TimeSignaturePoint {
                    at,
                    numerator: time_signature.numerator,
                    denominator: time_signature.denominator,
                });
            }
        }
        fits.then_some((tempo_map, time_signature_map))
    }

    /// The stem rules of [`crate::validate`] alone, for a draft refused
    /// before its maps were built: the package is checked with valid maps so
    /// only the stems can break a rule.
    fn stem_errors(
        &self,
        package: &SongPackage,
        infos: &[StemInfo],
        ids: &[String],
    ) -> Vec<DraftError> {
        let mut checked = package.clone();
        checked.tempo_map = vec![TempoPoint {
            at: 0,
            bpm: DEFAULT_IMPORT_BPM,
        }];
        checked.time_signature_map = vec![TimeSignaturePoint {
            at: 0,
            numerator: DEFAULT_IMPORT_TIME_SIGNATURE.numerator,
            denominator: DEFAULT_IMPORT_TIME_SIGNATURE.denominator,
        }];
        match validate(&checked, infos) {
            Ok(_) => Vec::new(),
            Err(rules) => rules
                .into_iter()
                .map(|error| self.error(error, ids))
                .collect(),
        }
    }

    /// The source file of the stem with id `id`.
    fn file_of(&self, id: &str, ids: &[String]) -> String {
        ids.iter()
            .position(|known| known == id)
            .map_or_else(|| id.to_owned(), |i| self.stems[i].source_name.clone())
    }

    /// A package rule broken, as a draft error naming the stems' files.
    fn error(&self, error: ValidationError, ids: &[String]) -> DraftError {
        match error {
            ValidationError::StemSampleRateMismatch {
                id,
                found,
                expected,
            } => DraftError::SampleRateMismatch {
                file: self.file_of(&id, ids),
                found,
                expected,
            },
            ValidationError::TooManyStemsWithRole { ids: ref named, .. } => {
                let files = named.iter().map(|id| self.file_of(id, ids)).collect();
                DraftError::Package { error, files }
            }
            ValidationError::SongTooLong { ref stem, .. } => {
                let files = vec![self.file_of(stem, ids)];
                DraftError::Package { error, files }
            }
            error => DraftError::Package {
                error,
                files: Vec::new(),
            },
        }
    }

    /// A package warning, naming the stems' files.
    fn warning(&self, warning: &ValidationWarning, ids: &[String]) -> DraftWarning {
        match warning {
            ValidationWarning::StemLengthsDiffer {
                shortest,
                shortest_length,
                longest,
                longest_length,
            } => DraftWarning::StemLengthsDiffer {
                shortest: self.file_of(shortest, ids),
                shortest_length: *shortest_length,
                longest: self.file_of(longest, ids),
                longest_length: *longest_length,
            },
        }
    }
}

fn check_bpm(bar: u64, bpm: f64, errors: &mut Vec<DraftError>) {
    if !bpm_in_range(bpm) {
        errors.push(DraftError::BpmOutOfRange { bar, bpm });
    }
}

fn check_time_signature(bar: u64, time_signature: TimeSignature, errors: &mut Vec<DraftError>) {
    let TimeSignature {
        numerator,
        denominator,
    } = time_signature;
    if !numerator_valid(numerator) || !denominator_valid(denominator) {
        errors.push(DraftError::InvalidTimeSignature {
            bar,
            numerator,
            denominator,
        });
    }
}

/// The sample of every bar line of a song of `length` frames with these
/// (valid) maps, from the [`BeatGrid`] the player uses: `bar_lines[n - 1]`
/// is where bar `n` starts.
fn bar_lines(
    sample_rate: u32,
    length: u64,
    tempo_map: &[TempoPoint],
    time_signature_map: &[TimeSignaturePoint],
) -> Vec<u64> {
    // The grid reads only the rate, the length and the maps.
    let song = Song {
        title: String::new(),
        artist: None,
        key: None,
        sample_rate,
        length,
        tempo_map: tempo_map.to_vec(),
        time_signature_map: time_signature_map.to_vec(),
        stems: Vec::new(),
        sections: Vec::new(),
        arrangements: Vec::new(),
    };
    BeatGrid::new(&song)
        .clicks_in(0..length)
        .iter()
        .filter(|click| click.level == ClickLevel::Downbeat)
        .map(|click| click.at)
        .collect()
}

/// `name` as an id: lowercase ASCII letters and digits without accents,
/// with one `-` for every run of anything else, at most [`MAX_ID_BYTES`]
/// long; `stem` when nothing is left.
fn slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separated = false;
    for c in name
        .chars()
        .flat_map(char::to_lowercase)
        .map(without_accent)
    {
        if c.is_ascii_alphanumeric() {
            if separated && !slug.is_empty() {
                slug.push('-');
            }
            separated = false;
            slug.push(c);
            if slug.len() >= MAX_ID_BYTES {
                break;
            }
        } else {
            separated = true;
        }
    }
    if slug.is_empty() {
        "stem".to_owned()
    } else {
        slug
    }
}

/// ` (files: "a", "b")` for a [`DraftError::Package`] that names files.
fn files_suffix(files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = files.iter().map(|file| format!("\"{file}\"")).collect();
    format!(" ({})", quoted.join(", "))
}

impl fmt::Display for StoredFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
        })
    }
}
