//! Song package format v1: the `song.json` manifest and its validation.
//!
//! "A song is a folder with a manifest and its stems." Positions (`at`,
//! `start`) are sample offsets at `sampleRate`. This module only sees the
//! manifest and the facts the library layer reads from the stem headers
//! ([`StemInfo`]); it never touches files.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::song::{Section, Song};

/// The only `formatVersion` this crate reads.
pub const FORMAT_VERSION: u32 = 1;

/// Id of the Default arrangement, which every package must contain.
pub const DEFAULT_ARRANGEMENT_ID: &str = "default";

/// Lowest accepted tempo. **Assumption:** bounding the tempo keeps click
/// generation finite.
pub const MIN_BPM: f64 = 1.0;

/// Highest accepted tempo. **Assumption:** see [`MIN_BPM`].
pub const MAX_BPM: f64 = 999.0;

/// The `song.json` manifest, exactly as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongPackage {
    /// Manifest format version; must be [`FORMAT_VERSION`].
    pub format_version: u32,
    /// Song title.
    pub title: String,
    /// Artist, optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Sample rate of every position in the manifest and of every stem.
    pub sample_rate: u32,
    /// Musical key, optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Tempo changes.
    pub tempo_map: Vec<TempoPoint>,
    /// Time-signature changes.
    pub time_signature_map: Vec<TimeSignaturePoint>,
    /// Audio stems.
    pub stems: Vec<Stem>,
    /// Section markers; a section ends where the next one starts.
    pub sections: Vec<SectionMarker>,
    /// Arrangements; one must have id [`DEFAULT_ARRANGEMENT_ID`].
    pub arrangements: Vec<Arrangement>,
}

/// A tempo that applies from `at` until the next tempo point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TempoPoint {
    /// Sample offset where the tempo starts.
    pub at: u64,
    /// Beats per minute, counting the time-signature denominator note.
    pub bpm: f64,
}

/// A time signature that applies from `at`; bars restart at `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignaturePoint {
    /// Sample offset where the time signature starts.
    pub at: u64,
    /// Beats per bar.
    pub numerator: u32,
    /// Note value of one beat (4 = quarter note).
    pub denominator: u32,
}

/// One audio stem of the package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stem {
    /// Stem id, unique within the package.
    pub id: String,
    /// Path of the audio file, relative to the song folder.
    pub file: String,
    /// Instrument part, such as `drums`.
    pub part: String,
    /// What the stem is used for.
    pub role: StemRole,
}

/// "`role` is `track`, `click`, or `guide`."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StemRole {
    /// A regular backing track.
    Track,
    /// A recorded click that replaces the generated one.
    Click,
    /// A recorded guide that replaces the generated cues.
    Guide,
}

/// A section marker as stored in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionMarker {
    /// Section id, unique within the package.
    pub id: String,
    /// What the section is.
    #[serde(rename = "type")]
    pub section_type: SectionType,
    /// Optional number, such as the 2 in "Verse 2".
    #[serde(default)]
    pub number: Option<u32>,
    /// Sample offset where the section starts.
    pub start: u64,
}

/// Section type, stored in kebab-case (`pre-chorus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(missing_docs)]
pub enum SectionType {
    CountOff,
    Intro,
    Verse,
    PreChorus,
    Chorus,
    PostChorus,
    Bridge,
    Instrumental,
    Interlude,
    Tag,
    Vamp,
    Turnaround,
    Breakdown,
    Outro,
    Ending,
}

impl SectionType {
    /// Every type, in declared order: the built-in list a section marker's
    /// type is chosen from (F2.2).
    pub const ALL: [SectionType; 15] = [
        SectionType::CountOff,
        SectionType::Intro,
        SectionType::Verse,
        SectionType::PreChorus,
        SectionType::Chorus,
        SectionType::PostChorus,
        SectionType::Bridge,
        SectionType::Instrumental,
        SectionType::Interlude,
        SectionType::Tag,
        SectionType::Vamp,
        SectionType::Turnaround,
        SectionType::Breakdown,
        SectionType::Outro,
        SectionType::Ending,
    ];

    /// The type as players and lists show it: "Count-off", "Pre-chorus".
    pub fn name(self) -> &'static str {
        match self {
            Self::CountOff => "Count-off",
            Self::Intro => "Intro",
            Self::Verse => "Verse",
            Self::PreChorus => "Pre-chorus",
            Self::Chorus => "Chorus",
            Self::PostChorus => "Post-chorus",
            Self::Bridge => "Bridge",
            Self::Instrumental => "Instrumental",
            Self::Interlude => "Interlude",
            Self::Tag => "Tag",
            Self::Vamp => "Vamp",
            Self::Turnaround => "Turnaround",
            Self::Breakdown => "Breakdown",
            Self::Outro => "Outro",
            Self::Ending => "Ending",
        }
    }

    /// A section of this type with `number` as players and lists show it:
    /// "Verse 2", or "Pre-chorus" without a number.
    pub fn label(self, number: Option<u32>) -> String {
        match number {
            Some(number) => format!("{} {number}", self.name()),
            None => self.name().to_owned(),
        }
    }
}

/// A named order of sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Arrangement {
    /// Arrangement id, unique within the package.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Section ids in play order; a section may repeat.
    pub sections: Vec<String>,
    /// Section ids marked as infinite loop by default (F2.4).
    #[serde(default)]
    pub infinite_loop: Vec<String>,
}

/// Facts the library layer reads from a stem file header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemInfo {
    /// Id of the stem this file belongs to.
    pub id: String,
    /// Sample rate of the file.
    pub sample_rate: u32,
    /// Number of sample frames in the file.
    pub length_samples: u64,
}

/// `song.json` could not be parsed or written.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// The manifest declares a `formatVersion` this crate cannot read; the
    /// rest of its shape is not looked at.
    #[error(
        "song.json: unsupported formatVersion {found}; this version reads formatVersion {FORMAT_VERSION}"
    )]
    UnsupportedFormatVersion {
        /// The version in the manifest.
        found: u32,
    },
    /// The JSON is malformed or does not have the v1 shape.
    #[error("song.json: {0}")]
    Json(serde_json::Error),
}

impl SongPackage {
    /// Parses a `song.json` manifest. `formatVersion` is checked first, so a
    /// newer manifest reports its version instead of a shape mismatch. Only the
    /// JSON shape is checked here; call [`validate`] for the package rules.
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        let header: VersionHeader = serde_json::from_str(json).map_err(ManifestError::Json)?;
        if header.format_version != FORMAT_VERSION {
            return Err(ManifestError::UnsupportedFormatVersion {
                found: header.format_version,
            });
        }
        serde_json::from_str(json).map_err(ManifestError::Json)
    }

    /// Writes the manifest as pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, ManifestError> {
        serde_json::to_string_pretty(self).map_err(ManifestError::Json)
    }
}

/// The one field every manifest version shares; read before the rest. Setlist
/// files share it too.
#[derive(Deserialize)]
pub(crate) struct VersionHeader {
    #[serde(rename = "formatVersion")]
    pub(crate) format_version: u32,
}

/// Longest accepted song, and longest accepted arrangement (every repeat
/// counted), in hours. **Assumption:** the cap keeps the click and plan sizes
/// bounded.
pub const MAX_SONG_HOURS: u64 = 24;

/// Why a `stems[].file` path is rejected: it must stay inside the song folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemPathProblem {
    /// The path is empty.
    Empty,
    /// The path starts at a root (`/`, `\`) or a drive (`C:`).
    Absolute,
    /// The path has a `..` component.
    ParentDirectory,
}

/// A validated package: the song plus non-fatal findings.
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    /// The validated song.
    pub song: Song,
    /// Findings that do not block playback.
    pub warnings: Vec<ValidationWarning>,
}

/// Which id namespace a duplicate id belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum IdKind {
    Stem,
    Section,
    Arrangement,
}

/// Which position map an error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum MapKind {
    Tempo,
    TimeSignature,
}

/// A rule the package breaks; [`validate`] reports all of them at once.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ValidationError {
    /// `formatVersion` is not [`FORMAT_VERSION`].
    #[error("unsupported formatVersion {found}; this version reads formatVersion {FORMAT_VERSION}")]
    UnsupportedFormatVersion {
        /// The version in the manifest.
        found: u32,
    },
    /// `sampleRate` is 0.
    #[error("sampleRate must be greater than 0")]
    InvalidSampleRate,
    /// Two stems, sections, or arrangements share an id.
    #[error("duplicate {kind} id \"{id}\"")]
    DuplicateId {
        /// Namespace of the id.
        kind: IdKind,
        /// The repeated id.
        id: String,
    },
    /// The package lists no stems, so it has no song end.
    #[error("the package lists no stems; the song end is the end of the longest stem")]
    NoStems,
    /// A stem file path is empty, absolute, or leaves the song folder.
    #[error(
        "stem \"{id}\" file \"{file}\" {problem}; it must be a relative path inside the song folder"
    )]
    InvalidStemPath {
        /// Stem id.
        id: String,
        /// The rejected path.
        file: String,
        /// What is wrong with it.
        problem: StemPathProblem,
    },
    /// The longest stem makes the song longer than [`MAX_SONG_HOURS`].
    #[error(
        "stem \"{stem}\" is {length} samples long; songs are limited to {MAX_SONG_HOURS} hours ({max} samples at the package sampleRate)"
    )]
    SongTooLong {
        /// Id of the longest stem.
        stem: String,
        /// Its length in samples.
        length: u64,
        /// The longest accepted length in samples.
        max: u64,
    },
    /// The library layer found no header facts for a listed stem.
    #[error("stem \"{id}\" ({file}) has no audio information; is the file missing or unreadable?")]
    MissingStemInfo {
        /// Stem id.
        id: String,
        /// Stem file path.
        file: String,
    },
    /// A stem's sample rate differs from the package (and so from the others).
    #[error(
        "stem \"{id}\" has sample rate {found} Hz, but the package sampleRate is {expected} Hz"
    )]
    StemSampleRateMismatch {
        /// Stem id.
        id: String,
        /// Sample rate of the stem file.
        found: u32,
        /// The package `sampleRate`.
        expected: u32,
    },
    /// More than one `click` or `guide` stem.
    #[error("only one {role} stem is allowed, found {}", quoted(ids))]
    TooManyStemsWithRole {
        /// The role that repeats.
        role: StemRole,
        /// Ids of every stem with that role.
        ids: Vec<String>,
    },
    /// A position map has no entries.
    #[error("{map} is empty; it needs an entry at 0")]
    EmptyMap {
        /// The empty map.
        map: MapKind,
    },
    /// A position map does not start at sample 0.
    #[error("{map} must start at 0, but its first entry is at {at}")]
    MapNotStartingAtZero {
        /// The map.
        map: MapKind,
        /// Position of its first entry.
        at: u64,
    },
    /// A position map entry is not after the previous one.
    #[error(
        "{map} entry at {at} is not after the previous entry at {previous_at}; positions must be strictly increasing"
    )]
    MapNotIncreasing {
        /// The map.
        map: MapKind,
        /// Position of the offending entry.
        at: u64,
        /// Position of the entry before it.
        previous_at: u64,
    },
    /// A tempo is not a finite number from [`MIN_BPM`] to [`MAX_BPM`].
    #[error("tempoMap entry at {at} has bpm {bpm}; bpm must be from {MIN_BPM} to {MAX_BPM}")]
    InvalidBpm {
        /// Position of the tempo point.
        at: u64,
        /// The rejected tempo.
        bpm: f64,
    },
    /// A time signature has numerator 0.
    #[error("timeSignatureMap entry at {at} has numerator 0; it must be at least 1")]
    InvalidNumerator {
        /// Position of the time-signature point.
        at: u64,
    },
    /// A time-signature denominator is not 1, 2, 4, 8, 16, or 32.
    #[error(
        "timeSignatureMap entry at {at} has denominator {denominator}; it must be 1, 2, 4, 8, 16, or 32"
    )]
    InvalidDenominator {
        /// Position of the time-signature point.
        at: u64,
        /// The rejected denominator.
        denominator: u32,
    },
    /// The package has no sections, so nothing can play.
    #[error("the package has no sections; at least one section is needed")]
    NoSections,
    /// The first section does not start at 0.
    #[error("the first section \"{id}\" starts at {start}; it must start at 0")]
    FirstSectionNotAtZero {
        /// Section id.
        id: String,
        /// Its start.
        start: u64,
    },
    /// A section does not start after the previous one (unsorted or equal starts).
    #[error(
        "section \"{id}\" starts at {start}, not after section \"{previous_id}\" at {previous_start}; section starts must be strictly increasing"
    )]
    SectionStartsNotIncreasing {
        /// Section id.
        id: String,
        /// Its start.
        start: u64,
        /// Id of the section before it.
        previous_id: String,
        /// Start of the section before it.
        previous_start: u64,
    },
    /// A section starts at or after the song end.
    #[error("section \"{id}\" starts at {start}, at or after the song end ({song_end})")]
    SectionStartBeyondSongEnd {
        /// Section id.
        id: String,
        /// Its start.
        start: u64,
        /// Song end: the length of the longest stem.
        song_end: u64,
    },
    /// No arrangement has id [`DEFAULT_ARRANGEMENT_ID`].
    #[error("the Default arrangement (id \"{DEFAULT_ARRANGEMENT_ID}\") is missing")]
    MissingDefaultArrangement,
    /// An arrangement lists no sections.
    #[error("arrangement \"{arrangement}\" has no sections")]
    EmptyArrangement {
        /// Arrangement id.
        arrangement: String,
    },
    /// An arrangement references a section id that does not exist.
    #[error("arrangement \"{arrangement}\" references unknown section \"{section}\"")]
    UnknownSectionReference {
        /// Arrangement id.
        arrangement: String,
        /// The unknown section id.
        section: String,
    },
    /// An arrangement plays longer than [`MAX_SONG_HOURS`] in total, counting
    /// every repeat of its sections.
    #[error(
        "arrangement \"{arrangement}\" plays longer than {MAX_SONG_HOURS} hours ({max} samples at the package sampleRate); arrangements are limited like songs"
    )]
    ArrangementTooLong {
        /// Arrangement id.
        arrangement: String,
        /// The longest accepted length in samples.
        max: u64,
    },
}

/// A finding that does not block playback.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationWarning {
    /// Stem lengths differ by more than one second (`sampleRate` samples).
    #[error(
        "stem lengths differ by more than one second: \"{shortest}\" has {shortest_length} samples, \"{longest}\" has {longest_length}"
    )]
    StemLengthsDiffer {
        /// Id of the shortest stem.
        shortest: String,
        /// Its length in samples.
        shortest_length: u64,
        /// Id of the longest stem.
        longest: String,
        /// Its length in samples.
        longest_length: u64,
    },
}

impl fmt::Display for IdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stem => "stem",
            Self::Section => "section",
            Self::Arrangement => "arrangement",
        })
    }
}

impl fmt::Display for MapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tempo => "tempoMap",
            Self::TimeSignature => "timeSignatureMap",
        })
    }
}

impl fmt::Display for StemPathProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "is empty",
            Self::Absolute => "is absolute",
            Self::ParentDirectory => "has a \"..\" component",
        })
    }
}

impl fmt::Display for StemRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Track => "track",
            Self::Click => "click",
            Self::Guide => "guide",
        })
    }
}

fn quoted(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks every package rule against the stem facts and returns all broken
/// rules at once, or the validated song with its warnings.
pub fn validate(
    package: &SongPackage,
    stem_infos: &[StemInfo],
) -> Result<Validated, Vec<ValidationError>> {
    let mut errors = Vec::new();
    if package.format_version != FORMAT_VERSION {
        errors.push(ValidationError::UnsupportedFormatVersion {
            found: package.format_version,
        });
    }
    if package.sample_rate == 0 {
        errors.push(ValidationError::InvalidSampleRate);
    }
    push_duplicates(
        IdKind::Stem,
        package.stems.iter().map(|s| s.id.as_str()),
        &mut errors,
    );
    push_duplicates(
        IdKind::Section,
        package.sections.iter().map(|s| s.id.as_str()),
        &mut errors,
    );
    push_duplicates(
        IdKind::Arrangement,
        package.arrangements.iter().map(|a| a.id.as_str()),
        &mut errors,
    );
    let stem_lengths = check_stems(package, stem_infos, &mut errors);
    let song_end = stem_lengths
        .as_ref()
        .map(|lengths| lengths.iter().map(|&(_, length)| length).max().unwrap_or(0));
    if let Some(lengths) = &stem_lengths {
        check_song_length(lengths, package.sample_rate, &mut errors);
    }
    check_tempo_map(&package.tempo_map, &mut errors);
    check_time_signature_map(&package.time_signature_map, &mut errors);
    check_sections(&package.sections, song_end, &mut errors);
    check_arrangements(package, &mut errors);
    if let Some(song_end) = song_end {
        check_arrangement_lengths(package, song_end, &mut errors);
    }

    match (stem_lengths, song_end) {
        (Some(lengths), Some(song_end)) if errors.is_empty() => Ok(Validated {
            song: build_song(package, song_end),
            warnings: length_warnings(&lengths, package.sample_rate),
        }),
        _ => Err(errors),
    }
}

fn push_duplicates<'a>(
    kind: IdKind,
    ids: impl Iterator<Item = &'a str>,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen = HashSet::new();
    let mut reported = HashSet::new();
    for id in ids {
        if !seen.insert(id) && reported.insert(id) {
            errors.push(ValidationError::DuplicateId {
                kind,
                id: id.to_owned(),
            });
        }
    }
}

/// Returns each stem's length when every stem has header facts.
fn check_stems<'a>(
    package: &'a SongPackage,
    infos: &[StemInfo],
    errors: &mut Vec<ValidationError>,
) -> Option<Vec<(&'a str, u64)>> {
    if package.stems.is_empty() {
        errors.push(ValidationError::NoStems);
        return None;
    }
    for role in [StemRole::Click, StemRole::Guide] {
        let ids: Vec<String> = package
            .stems
            .iter()
            .filter(|stem| stem.role == role)
            .map(|stem| stem.id.clone())
            .collect();
        if ids.len() > 1 {
            errors.push(ValidationError::TooManyStemsWithRole { role, ids });
        }
    }
    let mut lengths = Vec::with_capacity(package.stems.len());
    let mut complete = true;
    for stem in &package.stems {
        if let Some(problem) = stem_path_problem(&stem.file) {
            errors.push(ValidationError::InvalidStemPath {
                id: stem.id.clone(),
                file: stem.file.clone(),
                problem,
            });
        }
        let Some(info) = infos.iter().find(|info| info.id == stem.id) else {
            complete = false;
            errors.push(ValidationError::MissingStemInfo {
                id: stem.id.clone(),
                file: stem.file.clone(),
            });
            continue;
        };
        if info.sample_rate != package.sample_rate {
            errors.push(ValidationError::StemSampleRateMismatch {
                id: stem.id.clone(),
                found: info.sample_rate,
                expected: package.sample_rate,
            });
        }
        lengths.push((stem.id.as_str(), info.length_samples));
    }
    complete.then_some(lengths)
}

/// A stem path must stay inside the song folder on every platform, so both
/// `/` and `\` separate components and drive prefixes count as absolute. A
/// setlist's song paths follow the same rule inside the library folder.
pub(crate) fn stem_path_problem(file: &str) -> Option<StemPathProblem> {
    let bytes = file.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if file.is_empty() {
        Some(StemPathProblem::Empty)
    } else if drive || file.starts_with(['/', '\\']) {
        Some(StemPathProblem::Absolute)
    } else if file.split(['/', '\\']).any(|component| component == "..") {
        Some(StemPathProblem::ParentDirectory)
    } else {
        None
    }
}

/// Rejects a song longer than [`MAX_SONG_HOURS`], naming its first longest stem.
fn check_song_length(lengths: &[(&str, u64)], sample_rate: u32, errors: &mut Vec<ValidationError>) {
    // A zero sample rate is reported on its own; there is no meaningful cap.
    if sample_rate == 0 {
        return;
    }
    let max = MAX_SONG_HOURS * 3_600 * u64::from(sample_rate);
    let longest = lengths
        .iter()
        .copied()
        .reduce(|longest, stem| if stem.1 > longest.1 { stem } else { longest });
    if let Some((stem, length)) = longest
        && length > max
    {
        errors.push(ValidationError::SongTooLong {
            stem: stem.to_owned(),
            length,
            max,
        });
    }
}

fn check_positions(
    map: MapKind,
    positions: impl Iterator<Item = u64>,
    errors: &mut Vec<ValidationError>,
) {
    let mut previous = None;
    for at in positions {
        match previous {
            None if at != 0 => errors.push(ValidationError::MapNotStartingAtZero { map, at }),
            Some(previous_at) if at <= previous_at => {
                errors.push(ValidationError::MapNotIncreasing {
                    map,
                    at,
                    previous_at,
                });
            }
            _ => {}
        }
        previous = Some(at);
    }
    if previous.is_none() {
        errors.push(ValidationError::EmptyMap { map });
    }
}

/// Whether `bpm` is a finite tempo from [`MIN_BPM`] to [`MAX_BPM`]; NaN and
/// infinities fall outside the range. Setlist click tiles share the rule.
pub(crate) fn bpm_in_range(bpm: f64) -> bool {
    (MIN_BPM..=MAX_BPM).contains(&bpm)
}

/// Whether a time signature has at least one beat per bar. Setlist click tiles
/// share the rule.
pub(crate) fn numerator_valid(numerator: u32) -> bool {
    numerator >= 1
}

/// Whether a time-signature denominator is 1, 2, 4, 8, 16, or 32. Setlist
/// click tiles share the rule.
pub(crate) fn denominator_valid(denominator: u32) -> bool {
    matches!(denominator, 1 | 2 | 4 | 8 | 16 | 32)
}

/// The tempo map rules of [`validate`]. A grid built from bare maps
/// ([`crate::BeatGrid::from_maps`]) shares them.
pub(crate) fn check_tempo_map(map: &[TempoPoint], errors: &mut Vec<ValidationError>) {
    check_positions(MapKind::Tempo, map.iter().map(|point| point.at), errors);
    for point in map {
        if !bpm_in_range(point.bpm) {
            errors.push(ValidationError::InvalidBpm {
                at: point.at,
                bpm: point.bpm,
            });
        }
    }
}

/// The time-signature map rules of [`validate`]; see [`check_tempo_map`].
pub(crate) fn check_time_signature_map(
    map: &[TimeSignaturePoint],
    errors: &mut Vec<ValidationError>,
) {
    check_positions(
        MapKind::TimeSignature,
        map.iter().map(|point| point.at),
        errors,
    );
    for point in map {
        if !numerator_valid(point.numerator) {
            errors.push(ValidationError::InvalidNumerator { at: point.at });
        }
        if !denominator_valid(point.denominator) {
            errors.push(ValidationError::InvalidDenominator {
                at: point.at,
                denominator: point.denominator,
            });
        }
    }
}

fn check_sections(
    sections: &[SectionMarker],
    song_end: Option<u64>,
    errors: &mut Vec<ValidationError>,
) {
    if sections.is_empty() {
        errors.push(ValidationError::NoSections);
    }
    if let Some(first) = sections.first()
        && first.start != 0
    {
        errors.push(ValidationError::FirstSectionNotAtZero {
            id: first.id.clone(),
            start: first.start,
        });
    }
    for pair in sections.windows(2) {
        let (previous, section) = (&pair[0], &pair[1]);
        if section.start <= previous.start {
            errors.push(ValidationError::SectionStartsNotIncreasing {
                id: section.id.clone(),
                start: section.start,
                previous_id: previous.id.clone(),
                previous_start: previous.start,
            });
        }
    }
    // Without every stem length the song end is unknown; that is reported already.
    let Some(song_end) = song_end else {
        return;
    };
    for section in sections.iter().filter(|section| section.start >= song_end) {
        errors.push(ValidationError::SectionStartBeyondSongEnd {
            id: section.id.clone(),
            start: section.start,
            song_end,
        });
    }
}

fn check_arrangements(package: &SongPackage, errors: &mut Vec<ValidationError>) {
    if !package
        .arrangements
        .iter()
        .any(|arrangement| arrangement.id == DEFAULT_ARRANGEMENT_ID)
    {
        errors.push(ValidationError::MissingDefaultArrangement);
    }
    let known: HashSet<&str> = package.sections.iter().map(|s| s.id.as_str()).collect();
    for arrangement in &package.arrangements {
        if arrangement.sections.is_empty() {
            errors.push(ValidationError::EmptyArrangement {
                arrangement: arrangement.id.clone(),
            });
        }
        let mut reported = HashSet::new();
        for id in arrangement
            .sections
            .iter()
            .chain(&arrangement.infinite_loop)
        {
            if !known.contains(id.as_str()) && reported.insert(id.as_str()) {
                errors.push(ValidationError::UnknownSectionReference {
                    arrangement: arrangement.id.clone(),
                    section: id.clone(),
                });
            }
        }
    }
}

/// Rejects an arrangement that plays longer than [`MAX_SONG_HOURS`], so a
/// manifest repeating a long section cannot make `compile` exhaust memory. The
/// sum stops at the first section past the cap, so the check stays fast however
/// many sections the arrangement lists.
fn check_arrangement_lengths(
    package: &SongPackage,
    song_end: u64,
    errors: &mut Vec<ValidationError>,
) {
    let max = MAX_SONG_HOURS * 3_600 * u64::from(package.sample_rate);
    // A zero sample rate and a song over the cap are reported on their own.
    if package.sample_rate == 0 || song_end > max {
        return;
    }
    let markers = &package.sections;
    let lengths: HashMap<&str, u64> = markers
        .iter()
        .enumerate()
        .map(|(index, marker)| {
            let end = markers
                .get(index + 1)
                .map_or(song_end, |next| next.start.min(song_end));
            (marker.id.as_str(), end.saturating_sub(marker.start))
        })
        .collect();
    for arrangement in &package.arrangements {
        let mut total = 0u64;
        // Unknown section ids are reported by `check_arrangements`.
        for length in arrangement
            .sections
            .iter()
            .filter_map(|id| lengths.get(id.as_str()))
        {
            total = total.saturating_add(*length);
            if total > max {
                errors.push(ValidationError::ArrangementTooLong {
                    arrangement: arrangement.id.clone(),
                    max,
                });
                break;
            }
        }
    }
}

fn length_warnings(lengths: &[(&str, u64)], sample_rate: u32) -> Vec<ValidationWarning> {
    let shortest = lengths.iter().min_by_key(|&&(_, length)| length);
    let longest = lengths.iter().max_by_key(|&&(_, length)| length);
    match (shortest, longest) {
        (Some(&(shortest, shortest_length)), Some(&(longest, longest_length)))
            if longest_length - shortest_length > u64::from(sample_rate) =>
        {
            vec![ValidationWarning::StemLengthsDiffer {
                shortest: shortest.to_owned(),
                shortest_length,
                longest: longest.to_owned(),
                longest_length,
            }]
        }
        _ => Vec::new(),
    }
}

fn build_song(package: &SongPackage, song_end: u64) -> Song {
    let markers = &package.sections;
    let sections = markers
        .iter()
        .enumerate()
        .map(|(index, marker)| Section {
            id: marker.id.clone(),
            section_type: marker.section_type,
            number: marker.number,
            start: marker.start,
            end: markers.get(index + 1).map_or(song_end, |next| next.start),
        })
        .collect();
    Song {
        title: package.title.clone(),
        artist: package.artist.clone(),
        key: package.key.clone(),
        sample_rate: package.sample_rate,
        length: song_end,
        tempo_map: package.tempo_map.clone(),
        time_signature_map: package.time_signature_map.clone(),
        stems: package.stems.clone(),
        sections,
        arrangements: package.arrangements.clone(),
    }
}
