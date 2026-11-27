//! Song model and timeline compiler for the multitrack player.
//!
//! This crate is pure: it knows nothing about files, audio devices, or threads.
//! The library layer reads `song.json` and the stem headers, hands them to
//! [`validate`], and gets a [`Song`]. [`compile()`] turns a song, an arrangement,
//! and [`CompileOptions`] into a [`RenderPlan`] that the engine plays against a
//! single sample counter. A live player that walks an arrangement itself
//! queries bars and clicks in source time through [`BeatGrid`]. A [`Setlist`]
//! orders songs and click-only tiles; [`Setlist::validate`] checks it against
//! what a [`SongLookup`] knows about the library's songs.
//!
//! An import builds a [`SongDraft`] from the stems it finds; the user edits
//! its title, tempo, time signature, key and parts, and
//! [`SongDraft::to_package`] turns it into a checked [`SongPackage`]. Its
//! tempo and meter are a [`BarMaps`], which a saved song's maps read back as
//! when every change starts a bar.
//!
//! A [`SongEditor`] edits a saved song's section markers and arrangements
//! (F2): every edit leaves a package [`validate`] accepts, or leaves it
//! unchanged with a [`SongEditError`]. Markers snap to the bar or beat lines
//! of the song's [`BeatGrid`] ([`Snap`]).
//!
//! A stem's instrument is one of the fixed [`Part`]s: [`part_for_file`] picks
//! one from its file name by the rules table, and each part belongs to one
//! bus. Stems play through six [`Bus`]es: [`bus_for_stem`] picks one from the
//! same rules table, and an arrangement's [`ArrangementMix`] (stored in a song's
//! `mix.json`, a [`MixFile`]) can send a stem elsewhere. [`effective_gains`]
//! combines that mix with the global [`BusSettings`] into one gain per stem,
//! and a [`Routing`] sends each bus to a device's outputs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bus;
pub mod compile;
pub mod edit;
pub mod grid;
pub mod import;
pub mod mix;
pub mod mixer;
pub mod package;
pub mod part;
pub mod routing;
pub mod setlist;
pub mod song;
mod timeline;

pub use bus::{Bus, DEFAULT_BUS, bus_for_name, bus_for_stem};
pub use compile::{
    ClickEvent, ClickLevel, CompileError, CompileOptions, CueEvent, CueTiming,
    DEFAULT_SEAM_CROSSFADE, RenderPlan, Segment, compile, seam_fade,
};
pub use edit::{ArrangementSummary, Snap, SongEditError, SongEditor};
pub use grid::{BarBeat, BeatGrid, SourceClick};
pub use import::{
    BarMaps, BarMapsError, DEFAULT_ARRANGEMENT_NAME, DEFAULT_IMPORT_BPM,
    DEFAULT_IMPORT_TIME_SIGNATURE, DraftError, DraftStem, DraftWarning, INTRO_SECTION_ID,
    MapChange, PackagedDraft, STEMS_FOLDER, SongDraft, StoredFormat,
};
pub use mix::{
    ArrangementMix, MAX_VOLUME_DB, MIX_FORMAT_VERSION, MixFile, MixValidationError, MixerFile,
    MixerFileError, StemMix, Volume,
};
pub use mixer::{
    BUS_SETTINGS_FORMAT_VERSION, BusLevel, BusSettings, BusSettingsError, StemGain, effective_gains,
};
pub use package::{
    Arrangement, DEFAULT_ARRANGEMENT_ID, FORMAT_VERSION, IdKind, MAX_BPM, MAX_SONG_HOURS, MIN_BPM,
    ManifestError, MapKind, SectionMarker, SectionType, SongPackage, Stem, StemInfo,
    StemPathProblem, StemRole, TempoPoint, TimeSignaturePoint, Validated, ValidationError,
    ValidationWarning, validate,
};
pub use part::{Part, part_for_file, part_for_name};
pub use routing::{
    Destination, MAX_OUTPUTS, ROUTING_FORMAT_VERSION, RouteTarget, Routing, RoutingError,
    RoutingFile, RoutingValidationError,
};
pub use setlist::{
    ArrangementOutline, CROSSFADE_SECONDS, ClickItem, PositionBound, SETLIST_FORMAT_VERSION,
    SONG_FADE_OUT_SECONDS, Setlist, SetlistEditError, SetlistFileError, SetlistItem,
    SetlistValidationError, SongItem, SongLookup, SongOutline, TimeSignature, Transition,
};
pub use song::{CLICK_SECTION_ID, Section, Song};
