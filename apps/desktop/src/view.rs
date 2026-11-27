//! What the Perform screen shows, mapped from a session [`Snapshot`] and the
//! open song (S6): the song map with the playhead and the queued jump, the
//! section pads with Now and Next, the transport, the fade, the jump
//! timing, loop and infinite, the device chip, and what the notes say about
//! the device and the song. The design canvas's Main artboard adds what the
//! song's `song.json` says ([`SongFacts`]): its key, tempo and meter in the
//! song header, and the bars each section lasts on the map, the pads and
//! the Now and Next cards.

use std::ops::Range;

use app::{
    DeviceStatus, FadeState, JumpKind, JumpTiming, LoadError, LossReason, OpenError, PositionInfo,
    Snapshot, SongInfo, TransportState,
};
use domain::{DEFAULT_ARRANGEMENT_ID, ManifestError, SongPackage};

use crate::action::is_playing;
use crate::map_wave::SongWave;
use crate::song_sections::bar_height;

/// The family of a section, which picks its color on the song map and on
/// its pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// Count-off, intro, turnaround, outro, ending.
    Neutral,
    /// Verse.
    Verse,
    /// Pre-chorus.
    PreChorus,
    /// Chorus and post-chorus.
    Chorus,
    /// Bridge, instrumental, interlude, breakdown.
    Bridge,
    /// Tag and vamp.
    Tag,
}

impl SectionKind {
    /// The family of a section from its label, as [`PositionInfo::label`]
    /// writes it ("Verse 2", "Pre-chorus").
    pub fn of(label: &str) -> Self {
        match first_word(label) {
            "Verse" => Self::Verse,
            "Pre-chorus" => Self::PreChorus,
            "Chorus" | "Post-chorus" => Self::Chorus,
            "Bridge" | "Instrumental" | "Interlude" | "Breakdown" => Self::Bridge,
            "Tag" | "Vamp" => Self::Tag,
            _ => Self::Neutral,
        }
    }
}

/// A short name for a section block on the song map: "V2" for "Verse 2";
/// a tag without a number keeps its name, "Tag", as the design canvas has
/// it.
pub fn short_label(label: &str) -> String {
    let word = first_word(label);
    let number = label[word.len()..].trim();
    if word == "Tag" && number.is_empty() {
        return "Tag".to_owned();
    }
    let short = match word {
        "Count-off" => "CO",
        "Intro" => "In",
        "Verse" => "V",
        "Pre-chorus" => "PC",
        "Chorus" => "C",
        "Post-chorus" => "PoC",
        "Bridge" => "B",
        "Instrumental" => "Ins",
        "Interlude" => "Int",
        "Breakdown" => "Bd",
        "Tag" => "T",
        "Vamp" => "Vmp",
        "Turnaround" => "TA",
        "Outro" => "Out",
        "Ending" => "End",
        other => return other.chars().take(3).collect(),
    };
    format!("{short}{number}")
}

/// The names of `positions` on the song map, the pads and the Now and Next
/// cards, where a section without a number that plays more than once is
/// numbered by its turn: "Chorus" twice is "Chorus 1" and "Chorus 2", as
/// the design canvas writes them. A tag is the exception: "Tag" twice stays
/// "Tag" both times.
fn section_names(positions: &[PositionInfo]) -> Vec<String> {
    positions
        .iter()
        .enumerate()
        .map(|(index, info)| {
            let label = info.label.as_str();
            let repeats = positions.iter().filter(|p| p.label == label).count() > 1;
            if ends_in_number(label) || !repeats || label == "Tag" {
                return label.to_owned();
            }
            let turn = positions[..index]
                .iter()
                .filter(|p| p.label == label)
                .count()
                + 1;
            format!("{label} {turn}")
        })
        .collect()
}

/// The icon (a file of `assets/icons` without `.svg`) a section shows on
/// its pad and on the Now and Next cards, after the design canvas: its
/// family's (`SECTION_ICONS`), else a note.
pub fn section_icon(label: &str) -> &'static str {
    let word = first_word(label);
    SECTION_ICONS
        .iter()
        .find(|&&(first, _)| first == word)
        .map_or(OTHER_SECTION_ICON, |&(_, icon)| icon)
}

/// Every icon [`section_icon`] can return, and the end of the song's on
/// the Next card, for the app to carry each one.
pub fn section_icons() -> impl Iterator<Item = &'static str> {
    SECTION_ICONS
        .iter()
        .map(|&(_, icon)| icon)
        .chain([OTHER_SECTION_ICON, END_ICON])
}

/// The icon of each section family, by the first word of its label.
const SECTION_ICONS: &[(&str, &str)] = &[
    ("Count-off", "metronome"),
    ("Intro", "enter"),
    ("Verse", "lines"),
    ("Pre-chorus", "rise"),
    ("Chorus", "star"),
    ("Post-chorus", "star"),
    ("Bridge", "bridge"),
    ("Breakdown", "bridge"),
    ("Tag", "tag"),
    ("Vamp", "loop"),
    ("Turnaround", "turn"),
    ("Outro", "exit"),
    ("Ending", "flag"),
];

/// The icon of a section whose family has none of its own (Instrumental,
/// Interlude).
const OTHER_SECTION_ICON: &str = "note";

/// The icon of the end of the song on the Next card.
const END_ICON: &str = "flag";

/// "1 bar", "16 bars".
fn bars_text(bars: u64) -> String {
    if bars == 1 {
        "1 bar".to_owned()
    } else {
        format!("{bars} bars")
    }
}

fn first_word(label: &str) -> &str {
    label.split(' ').next().unwrap_or(label)
}

/// Whether `label`'s last word is a number, as in "Verse 2".
fn ends_in_number(label: &str) -> bool {
    label
        .rsplit_once(' ')
        .is_some_and(|(_, last)| last.parse::<u32>().is_ok())
}

/// One arrangement position on the song map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapBlock {
    /// Index into the arrangement.
    pub position: usize,
    /// Full name, such as "Verse 2", or "Chorus 2" for the second
    /// "Chorus".
    pub label: String,
    /// Short label, such as "V2".
    pub short: String,
    /// Section family.
    pub kind: SectionKind,
    /// Where the block starts, as a fraction of the arrangement, 0.0 to 1.0.
    pub start: f32,
    /// Its width, as a fraction of the arrangement.
    pub width: f32,
    /// Whether the playhead is in it.
    pub is_now: bool,
    /// Whether it is the target of the queued jump or loop.
    pub is_queued: bool,
    /// How many bars it lasts, when the song says.
    pub bars: Option<u64>,
    /// The ruler's number under it: the bar it starts at, counted in play
    /// order from 1; `None` for a block shorter than 4 bars, which has no
    /// room for it, or when the bars before it cannot be counted.
    pub ruler: Option<String>,
}

/// Where and when the queued jump or loop executes (F6.5).
#[derive(Debug, Clone, PartialEq)]
pub struct JumpMarker {
    /// Where it executes, as a fraction of the arrangement.
    pub at: f32,
    /// What it is and when it executes, such as "Jump · in 4.2 s".
    pub label: String,
}

/// The share of the song map's track the quietest bar of its waveform
/// fills, as on the design canvas: 18 %.
pub const MAP_WAVE_LOW: f32 = 0.18;

/// The share of the track the loudest bar fills: 96 %.
pub const MAP_WAVE_HIGH: f32 = 0.96;

/// The opacity of the waveform's bars ahead of the playhead, as on the
/// design canvas; the bars it has reached are opaque.
pub const AHEAD_OPACITY: f32 = 0.38;

/// One bar of the waveform on the song map's track: a musical bar of a
/// position, or the part of one it holds when the section starts or ends
/// inside a bar.
#[derive(Debug, Clone, PartialEq)]
pub struct MapBar {
    /// Its position, an index into the arrangement.
    pub position: usize,
    /// Its section's family, which picks its color.
    pub kind: SectionKind,
    /// The source frames it shows: its position's own audio.
    pub source: Range<u64>,
    /// Where it starts, as a fraction of the arrangement.
    pub start: f32,
    /// Its width, as a fraction of the arrangement.
    pub width: f32,
    /// Its height, as a share of the track's: its level in the song's
    /// overview over the song's peak, from [`MAP_WAVE_LOW`] to
    /// [`MAP_WAVE_HIGH`].
    pub height: f32,
    /// 1 once the playhead is past its start (played), else
    /// [`AHEAD_OPACITY`].
    pub opacity: f32,
}

/// The song map: the arrangement in play order, the playhead, and the
/// queued jump.
#[derive(Debug, Clone, PartialEq)]
pub struct SongMap {
    /// The arrangement's positions, in play order.
    pub blocks: Vec<MapBlock>,
    /// The playhead, as a fraction of the arrangement.
    pub playhead: f32,
    /// The queued jump or loop, if any.
    pub jump: Option<JumpMarker>,
    /// The waveform on the track, in play order: each position's bars in
    /// its block. Empty until the song's waveform is read, or when it
    /// cannot be: the track then shows the blocks' colors.
    pub wave: Vec<MapBar>,
}

/// One section pad.
#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    /// Index into the arrangement; the pad queues a jump to it.
    pub position: usize,
    /// Its name, as the song map names it ([`MapBlock::label`]).
    pub label: String,
    /// Its short name, large on the pad: "V2", or "C2" for the second
    /// "Chorus" ([`MapBlock::short`]).
    pub abbr: String,
    /// Its icon ([`section_icon`]).
    pub icon: &'static str,
    /// How many bars it lasts, such as "16 bars", when the song says.
    pub bars: Option<String>,
    /// Section family.
    pub kind: SectionKind,
    /// The key that queues it, for the first nine positions.
    pub key: Option<char>,
    /// Marked Now: the playhead is in it.
    pub is_now: bool,
    /// Marked Next: it plays after the current section.
    pub is_next: bool,
    /// It plays next because a jump or loop to it is queued.
    pub is_queued: bool,
}

/// The card that says what is playing.
#[derive(Debug, Clone, PartialEq)]
pub struct NowCard {
    /// The current section, as the song map names it
    /// ([`MapBlock::label`]).
    pub name: String,
    /// Time left in it, such as "6.3 s left".
    pub detail: String,
    /// Bars left in it, counting the one playing, such as "11 bars left";
    /// `None` when its bars cannot be counted.
    pub bars_left: Option<String>,
    /// Its family, which colors its icon.
    pub kind: SectionKind,
    /// Its icon ([`section_icon`]).
    pub icon: &'static str,
}

/// The card that says what plays next.
#[derive(Debug, Clone, PartialEq)]
pub struct NextCard {
    /// "Next", "Jump queued" or "Loop queued".
    pub heading: String,
    /// The section that plays next, as the song map names it
    /// ([`MapBlock::label`]), or "End of song".
    pub name: String,
    /// When, such as "Executes in 4.2 s".
    pub detail: String,
    /// Whether a jump or loop is queued; the card then offers to cancel it.
    pub queued: bool,
    /// [`Self::detail`] in bars, such as "Plays in 11 bars · tap a section
    /// to jump"; `None` for a queued jump, which says its time, and when
    /// the current section's bars cannot be counted.
    pub bars_detail: Option<String>,
    /// The family of what plays next; neutral for the end of the song.
    pub kind: SectionKind,
    /// Its icon ([`section_icon`]), or a flag for the end of the song.
    pub icon: &'static str,
}

/// The transport.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportView {
    /// What the transport does, such as "Playing" or "Loading…".
    pub state: String,
    /// Label of the play/pause button: "Pause" while playing or loading,
    /// else "Play".
    pub play_label: String,
    /// Whether the play/pause button shows pause.
    pub playing: bool,
    /// What the clock adds while the transport changes state ("Loading…",
    /// "Stopping", "Panic"), which the play/pause icon does not show;
    /// `None` while it plays, is paused or stopped.
    pub note: Option<String>,
}

/// The fade button.
#[derive(Debug, Clone, PartialEq)]
pub struct FadeButton {
    /// "Fade", "Fading out 40%", "Fade in" or "Fading in 60%".
    pub label: String,
    /// Whether the stems are faded or fading.
    pub active: bool,
}

/// How the device chip's dot reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// All is well.
    Ok,
    /// Plays, with something to look at: underruns.
    Warning,
    /// Needs attention.
    Error,
}

/// What the notes under the strip say about the device and the song, in
/// the `app` API's own words; all `None` and empty while all is well. The
/// device itself and its underruns are on the header's [`DeviceChip`].
#[derive(Debug, Clone, PartialEq)]
pub struct StatusView {
    /// Why the device plays nothing: no device, or one that cannot be
    /// opened again or refuses new settings; `None` while it plays.
    pub device_problem: Option<String>,
    /// The last problem ([`Snapshot::last_error`]), as the `app` API words it.
    pub last_error: Option<String>,
    /// Findings in the song package that do not block playback.
    pub warnings: Vec<String>,
    /// Why the song header has no key, tempo or meter: its `song.json`
    /// could not be read ([`SongFacts::unread`]).
    pub facts_problem: Option<String>,
}

/// The header's device chip: the device, its outputs, buffer and latency.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceChip {
    /// The device's name.
    pub name: String,
    /// Its outputs, buffer frames and latency, such as "8 out · 256 ·
    /// 5.3 ms", with the underruns when there are any; "Disconnected" or
    /// "Refuses the new settings" without a stream (the status says why).
    pub meta: String,
    /// [`Level::Warning`] with underruns, [`Level::Error`] without a
    /// stream.
    pub level: Level,
}

/// The line under the song's title: its key, tempo and meter, and the
/// arrangement playing.
#[derive(Debug, Clone, PartialEq)]
pub struct SongHeader {
    /// Key, such as "G".
    pub key: Option<String>,
    /// Tempo the song starts in, such as "104 BPM".
    pub tempo: Option<String>,
    /// Time signature the song starts in, such as "4/4".
    pub meter: Option<String>,
    /// The arrangement playing, by its name ("Default"), or by its id when
    /// the song's facts do not name it.
    pub arrangement: String,
}

/// What a song's `song.json` (or a click-only tile) says that the session
/// does not: its key, tempo and meter, its arrangements' names, and how
/// long a bar lasts, which counts the bars of a section no other starts
/// after. Empty until the file is read (`crate::facts`), and with
/// [`Self::unread`] while it cannot be.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SongFacts {
    /// Key, such as "G".
    pub key: Option<String>,
    /// Tempo at the start, in beats per minute.
    pub bpm: Option<f64>,
    /// Time signature at the start: beats per bar and the note of a beat.
    pub meter: Option<(u32, u32)>,
    /// Every arrangement, as (id, name).
    pub arrangements: Vec<(String, String)>,
    /// From each tempo or time-signature change on (its source frame), the
    /// seconds a bar lasts; in source order.
    pub bar_seconds: Vec<(u64, f64)>,
    /// Why the song's `song.json` could not be read, the note the notes
    /// show while it cannot; the other facts are then empty.
    pub unread: Option<String>,
}

impl SongFacts {
    /// The facts of the `song.json` manifest `json`; `None` when it does not
    /// parse ([`Self::parse`]).
    pub fn from_manifest(json: &str) -> Option<Self> {
        Self::parse(json).ok()
    }

    /// The facts of the `song.json` manifest `json`, or why it does not
    /// parse ([`SongPackage::from_json`]).
    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        let package = SongPackage::from_json(json)?;
        let mut changes: Vec<u64> = package
            .tempo_map
            .iter()
            .map(|point| point.at)
            .chain(package.time_signature_map.iter().map(|point| point.at))
            .collect();
        changes.sort_unstable();
        changes.dedup();
        // The tempo counts the time signature's note: a bar is `numerator`
        // of them.
        let bar_seconds = changes
            .into_iter()
            .filter_map(|at| {
                let bpm = package.tempo_map.iter().rfind(|p| p.at <= at)?.bpm;
                let meter = package.time_signature_map.iter().rfind(|p| p.at <= at)?;
                (bpm > 0.0).then(|| (at, 60.0 / bpm * f64::from(meter.numerator)))
            })
            .collect();
        Ok(Self {
            key: package.key.clone(),
            bpm: package.tempo_map.first().map(|point| point.bpm),
            meter: package
                .time_signature_map
                .first()
                .map(|point| (point.numerator, point.denominator)),
            arrangements: package
                .arrangements
                .iter()
                .map(|arrangement| (arrangement.id.clone(), arrangement.name.clone()))
                .collect(),
            bar_seconds,
            unread: None,
        })
    }

    /// The facts of a click-only tile at `bpm`, in `numerator`/`denominator`;
    /// its one arrangement is named as `domain::Song::click_only` names it.
    pub fn click(bpm: f64, numerator: u32, denominator: u32) -> Self {
        Self {
            key: None,
            bpm: Some(bpm),
            meter: Some((numerator, denominator)),
            arrangements: vec![(DEFAULT_ARRANGEMENT_ID.to_owned(), "Default".to_owned())],
            bar_seconds: if bpm > 0.0 {
                vec![(0, 60.0 / bpm * f64::from(numerator))]
            } else {
                Vec::new()
            },
            unread: None,
        }
    }

    /// The frames a bar lasts at source frame `at`, at `rate`.
    fn bar_frames_at(&self, at: u64, rate: u32) -> Option<f64> {
        self.bar_seconds
            .iter()
            .rfind(|(from, _)| *from <= at)
            .map(|(_, seconds)| seconds * f64::from(rate))
    }
}

/// How the section pads lay out in a row `width` points wide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PadGrid {
    /// Pads per row, which share its width.
    pub columns: usize,
    /// A pad's height, in points.
    pub height: f32,
    /// More than [`COMPACT_PADS_AFTER`] pads: shorter pads, a smaller
    /// name, and no bar count.
    pub compact: bool,
}

/// Past this many pads, they get compact ([`PadGrid::compact`]).
pub const COMPACT_PADS_AFTER: usize = 12;

/// The room between pads, in points.
pub const PAD_GAP: f32 = 8.0;

/// The pads' grid for `pads` pads in a row `width` points wide, as the
/// design canvas lays them out: pads at least 100 points wide (92 when
/// compact) share the row, as many as fit, and wrap to more rows.
pub fn pad_grid(width: f32, pads: usize) -> PadGrid {
    let compact = pads > COMPACT_PADS_AFTER;
    let (min, height) = if compact {
        (92.0, 46.0)
    } else {
        (100.0, 100.0)
    };
    let fit = ((width + PAD_GAP) / (min + PAD_GAP)).floor().max(1.0) as usize;
    PadGrid {
        columns: fit.min(pads).max(1),
        height,
        compact,
    }
}

/// Everything the Perform screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PerformView {
    /// Song title.
    pub title: String,
    /// The line under the title: key, tempo, meter and arrangement.
    pub header: SongHeader,
    /// The header's device chip.
    pub device: DeviceChip,
    /// The transport.
    pub transport: TransportView,
    /// Time in the arrangement, such as "01:25".
    pub time: String,
    /// The arrangement's length, such as "05:14".
    pub total: String,
    /// Bar and beat, such as "Bar 38 · beat 2".
    pub bar_beat: String,
    /// The song map.
    pub map: SongMap,
    /// What plays now.
    pub now: NowCard,
    /// What plays next.
    pub next: NextCard,
    /// The section pads, in arrangement order.
    pub pads: Vec<Pad>,
    /// The jump timing the toggle shows.
    pub jump_timing: JumpTiming,
    /// Whether a live loop is queued ("Loop section" shows pressed).
    pub live_loop: bool,
    /// Whether the infinite loop is on.
    pub infinite: bool,
    /// The fade button.
    pub fade: FadeButton,
    /// What the notes say about the device and the song.
    pub status: StatusView,
}

impl PerformView {
    /// What the screen shows for `snapshot` of `song`, knowing nothing of
    /// its `song.json` ([`Self::with_facts`]).
    pub fn new(snapshot: &Snapshot, song: &SongInfo) -> Self {
        Self::with_facts(snapshot, song, &SongFacts::default())
    }

    /// What the screen shows for `snapshot` of `song`, whose `song.json`
    /// says `facts`, before its waveform is read ([`Self::with_wave`]).
    pub fn with_facts(snapshot: &Snapshot, song: &SongInfo, facts: &SongFacts) -> Self {
        Self::with_wave(snapshot, song, facts, None)
    }

    /// What the screen shows for `snapshot` of `song`, whose `song.json`
    /// says `facts`, with its waveform `wave` on the song map once it is
    /// read ([`crate::map_wave`]).
    pub fn with_wave(
        snapshot: &Snapshot,
        song: &SongInfo,
        facts: &SongFacts,
        wave: Option<&SongWave>,
    ) -> Self {
        let timeline = Timeline::new(snapshot, song);
        let next = next_position(snapshot, song);
        let queued_target = snapshot.queued.as_ref().map(|queued| queued.target);
        let bars = position_bars(song, facts);
        let names = section_names(&song.positions);
        let abbreviations: Vec<String> = names.iter().map(|name| short_label(name)).collect();
        let pads = song
            .positions
            .iter()
            .enumerate()
            .map(|(position, info)| Pad {
                position,
                label: names[position].clone(),
                abbr: abbreviations[position].clone(),
                icon: section_icon(&info.label),
                bars: bars[position].map(bars_text),
                kind: SectionKind::of(&info.label),
                key: char::from_digit(position as u32 + 1, 10).filter(|_| position < 9),
                is_now: position == snapshot.position,
                is_next: next == Some(position),
                is_queued: queued_target == Some(position),
            })
            .collect();
        let bars_left = bars_left(snapshot, song, &bars);
        Self {
            title: song.title.clone(),
            header: song_header(song, facts),
            device: device_chip(snapshot),
            transport: transport_view(snapshot.transport),
            time: clock(timeline.elapsed, song.sample_rate),
            total: clock(timeline.total, song.sample_rate),
            bar_beat: format!("Bar {} · beat {}", snapshot.bar, snapshot.beat),
            map: SongMap {
                wave: wave.map_or_else(Vec::new, |wave| map_wave(song, &timeline, wave)),
                ..song_map(snapshot, song, &timeline, &names, &abbreviations, &bars)
            },
            now: NowCard {
                name: names
                    .get(snapshot.position)
                    .cloned()
                    .unwrap_or_else(|| snapshot.section.clone()),
                detail: format!(
                    "{} left",
                    seconds(timeline.left_in_section, song.sample_rate)
                ),
                bars_left: bars_left.map(|left| format!("{} left", bars_text(left))),
                kind: SectionKind::of(&snapshot.section),
                icon: section_icon(&snapshot.section),
            },
            next: next_card(
                snapshot,
                song,
                &names,
                next,
                timeline.left_in_section,
                bars_left,
            ),
            pads,
            jump_timing: snapshot.jump_timing,
            live_loop: snapshot.live_loop(),
            infinite: snapshot.infinite,
            fade: fade_button(snapshot),
            status: status_view(snapshot, song, facts),
        }
    }
}

/// Where the playhead is along the arrangement, in source frames played in
/// arrangement order.
struct Timeline {
    /// Frames of every position.
    total: u64,
    /// Frames from the start of the arrangement to the playhead.
    elapsed: u64,
    /// Frames from the playhead to the end of the current position.
    left_in_section: u64,
}

impl Timeline {
    fn new(snapshot: &Snapshot, song: &SongInfo) -> Self {
        let total = song.positions.iter().map(|p| p.length_frames).sum();
        let before: u64 = song
            .positions
            .iter()
            .take(snapshot.position)
            .map(|p| p.length_frames)
            .sum();
        let (offset, length) =
            song.positions
                .get(snapshot.position)
                .map_or((0, 0), |current: &PositionInfo| {
                    let offset = snapshot.source_frame.saturating_sub(current.start_frame);
                    (offset.min(current.length_frames), current.length_frames)
                });
        Self {
            total,
            elapsed: before + offset,
            left_in_section: length - offset,
        }
    }

    /// `frames` as a fraction of the arrangement.
    fn fraction(&self, frames: u64) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (frames as f64 / self.total as f64) as f32
        }
    }
}

/// How many bars each position of `song` lasts: up to the bar the section
/// that follows it in the song starts at, when one in the arrangement
/// does; else its length in bars at the tempo and meter it starts in, from
/// `facts`, a bar begun counting as one. `None` when neither says.
fn position_bars(song: &SongInfo, facts: &SongFacts) -> Vec<Option<u64>> {
    song.positions
        .iter()
        .map(|info| {
            let end = info.start_frame + info.length_frames;
            let following = song
                .positions
                .iter()
                .find(|other| other.start_frame == end && other.start_bar > info.start_bar);
            if let Some(following) = following {
                return Some(following.start_bar - info.start_bar);
            }
            let bar = facts.bar_frames_at(info.start_frame, song.sample_rate)?;
            // A rounding of the frames never makes a bar of its own.
            let bars = (info.length_frames as f64 / bar - 1e-6).ceil();
            (bars.is_finite() && bars >= 0.0).then(|| (bars as u64).max(1))
        })
        .collect()
}

/// Bars left in the current position, counting the one playing; `None`
/// when its bars cannot be counted.
fn bars_left(snapshot: &Snapshot, song: &SongInfo, bars: &[Option<u64>]) -> Option<u64> {
    let info = song.positions.get(snapshot.position)?;
    let bars = (*bars.get(snapshot.position)?)?;
    let into = snapshot.bar.saturating_sub(info.start_bar);
    Some(bars.saturating_sub(into).clamp(1, bars.max(1)))
}

/// The bar each position starts at, counted in play order from 1, for the
/// positions of 4 bars or more whose preceding bars are all counted.
fn ruler(bars: &[Option<u64>]) -> Vec<Option<String>> {
    let mut start = Some(1);
    bars.iter()
        .map(|bars| {
            let label = start
                .zip(*bars)
                .filter(|(_, bars)| *bars >= 4)
                .map(|(start, _)| start.to_string());
            start = start.zip(*bars).map(|(start, bars)| start + bars);
            label
        })
        .collect()
}

fn song_map(
    snapshot: &Snapshot,
    song: &SongInfo,
    timeline: &Timeline,
    names: &[String],
    abbreviations: &[String],
    bars: &[Option<u64>],
) -> SongMap {
    let mut start = 0;
    let ruler = ruler(bars);
    let blocks = song
        .positions
        .iter()
        .enumerate()
        .map(|(position, info)| {
            let block = MapBlock {
                position,
                label: names[position].clone(),
                short: abbreviations[position].clone(),
                kind: SectionKind::of(&info.label),
                start: timeline.fraction(start),
                width: timeline.fraction(info.length_frames),
                is_now: position == snapshot.position,
                is_queued: snapshot
                    .queued
                    .as_ref()
                    .is_some_and(|q| q.target == position),
                bars: bars[position],
                ruler: ruler[position].clone(),
            };
            start += info.length_frames;
            block
        })
        .collect();
    // Output and source frames advance together within a section, so the
    // jump executes `frames_until` past the playhead, and never past the end
    // of the section it leaves.
    let jump = snapshot.queued.as_ref().map(|queued| {
        let ahead = queued.frames_until.min(timeline.left_in_section);
        JumpMarker {
            at: timeline.fraction(timeline.elapsed + ahead),
            label: format!(
                "{} · in {}",
                kind_name(queued.kind),
                seconds(queued.frames_until, song.sample_rate)
            ),
        }
    });
    SongMap {
        blocks,
        playhead: timeline.fraction(timeline.elapsed),
        jump,
        wave: Vec::new(),
    }
}

/// The waveform on the song map's track: each position's musical bars
/// (the bar lines of `wave` inside its source frames, cut where the
/// section starts or ends inside a bar), each as tall as the level of its
/// own frames, in play order; opaque once the playhead is past its start.
fn map_wave(song: &SongInfo, timeline: &Timeline, wave: &SongWave) -> Vec<MapBar> {
    let lines = wave.bar_lines();
    let mut before = 0;
    let mut bars = Vec::new();
    for (position, info) in song.positions.iter().enumerate() {
        let kind = SectionKind::of(&info.label);
        let end = info.start_frame + info.length_frames;
        let first = lines.partition_point(|&line| line <= info.start_frame);
        let inside = lines[first..]
            .iter()
            .copied()
            .take_while(|&line| line < end);
        let mut from = info.start_frame;
        for to in inside.chain([end]) {
            if to > from {
                let at = before + (from - info.start_frame);
                bars.push(MapBar {
                    position,
                    kind,
                    source: from..to,
                    start: timeline.fraction(at),
                    width: timeline.fraction(to - from),
                    height: bar_height(wave.overview(), from..to, MAP_WAVE_LOW..MAP_WAVE_HIGH),
                    opacity: if at < timeline.elapsed {
                        1.0
                    } else {
                        AHEAD_OPACITY
                    },
                });
            }
            from = to;
        }
        before += info.length_frames;
    }
    bars
}

/// The position that plays after the current section: the queued target,
/// the current one again under the infinite loop, else the next one;
/// `None` at the end of the song.
fn next_position(snapshot: &Snapshot, song: &SongInfo) -> Option<usize> {
    if let Some(queued) = &snapshot.queued {
        return Some(queued.target);
    }
    if snapshot.infinite {
        return Some(snapshot.position);
    }
    let next = snapshot.position + 1;
    (next < song.positions.len()).then_some(next)
}

fn next_card(
    snapshot: &Snapshot,
    song: &SongInfo,
    names: &[String],
    next: Option<usize>,
    left_in_section: u64,
    bars_left: Option<u64>,
) -> NextCard {
    let rate = song.sample_rate;
    let name = |position: usize| names.get(position).cloned().unwrap_or_default();
    let (kind, icon) = match next.and_then(|position| song.positions.get(position)) {
        Some(info) => (SectionKind::of(&info.label), section_icon(&info.label)),
        None => (SectionKind::Neutral, END_ICON),
    };
    if let Some(queued) = &snapshot.queued {
        let mut detail = format!("Executes in {}", seconds(queued.frames_until, rate));
        if queued.deferrals > 0 {
            let plural = if queued.deferrals == 1 { "y" } else { "ies" };
            detail += &format!(
                " · moved on {} boundar{plural} waiting for its audio",
                queued.deferrals
            );
        }
        return NextCard {
            heading: format!("{} queued", kind_name(queued.kind)),
            name: name(queued.target),
            detail,
            queued: true,
            bars_detail: None,
            kind,
            icon,
        };
    }
    // When it comes, in seconds and in bars.
    let when = |left: &str| match next {
        Some(_) if snapshot.infinite => format!("Repeats in {left} · infinite loop"),
        Some(_) => format!("Plays in {left} · tap a section to jump"),
        None => format!("Ends in {left}"),
    };
    NextCard {
        heading: "Next".to_owned(),
        name: next.map_or_else(|| "End of song".to_owned(), name),
        detail: when(&seconds(left_in_section, rate)),
        queued: false,
        bars_detail: bars_left.map(|left| when(&bars_text(left))),
        kind,
        icon,
    }
}

/// The header's key, tempo and meter from `facts`, and the arrangement by
/// the name `facts` give it.
fn song_header(song: &SongInfo, facts: &SongFacts) -> SongHeader {
    SongHeader {
        key: facts.key.clone(),
        tempo: facts.bpm.map(|bpm| format!("{bpm} BPM")),
        meter: facts
            .meter
            .map(|(numerator, denominator)| format!("{numerator}/{denominator}")),
        arrangement: facts
            .arrangements
            .iter()
            .find(|(id, _)| *id == song.arrangement)
            .map_or_else(|| song.arrangement.clone(), |(_, name)| name.clone()),
    }
}

/// The device chip: the stream's device, outputs, buffer and latency, and
/// the underruns so far; without a stream, the device and that it plays
/// nothing.
fn device_chip(snapshot: &Snapshot) -> DeviceChip {
    match &snapshot.device {
        DeviceStatus::Connected(info) => {
            let mut meta = format!(
                "{} out · {} · {:.1} ms",
                info.channels,
                info.buffer_frames,
                info.latency().as_secs_f64() * 1_000.0
            );
            let level = match snapshot.underruns {
                0 => Level::Ok,
                1 => {
                    meta += " · 1 underrun";
                    Level::Warning
                }
                underruns => {
                    meta += &format!(" · {underruns} underruns");
                    Level::Warning
                }
            };
            DeviceChip {
                name: info.device.clone(),
                meta,
                level,
            }
        }
        DeviceStatus::Disconnected {
            device,
            reason: LossReason::Reconfigured,
            reopen_error: Some(_),
        } => DeviceChip {
            name: device.clone(),
            meta: "Refuses the new settings".to_owned(),
            level: Level::Error,
        },
        DeviceStatus::Disconnected { device, .. } | DeviceStatus::PlayerLost { device } => {
            DeviceChip {
                name: device.clone(),
                meta: "Disconnected".to_owned(),
                level: Level::Error,
            }
        }
    }
}

fn kind_name(kind: JumpKind) -> &'static str {
    match kind {
        JumpKind::Jump => "Jump",
        JumpKind::Loop => "Loop",
    }
}

fn transport_view(transport: TransportState) -> TransportView {
    let state = match transport {
        TransportState::Stopped => "Stopped",
        TransportState::Starting => "Loading…",
        TransportState::Playing => "Playing",
        TransportState::Pausing | TransportState::Paused => "Paused",
        TransportState::Stopping => "Stopping",
        TransportState::Panicking => "Panic",
    };
    let playing = is_playing(transport);
    let note = matches!(
        transport,
        TransportState::Starting | TransportState::Stopping | TransportState::Panicking
    )
    .then(|| state.to_owned());
    TransportView {
        state: state.to_owned(),
        play_label: if playing { "Pause" } else { "Play" }.to_owned(),
        playing,
        note,
    }
}

fn fade_button(snapshot: &Snapshot) -> FadeButton {
    let percent = (snapshot.fade.level * 100.0).round();
    let label = match snapshot.fade.state {
        FadeState::In => "Fade".to_owned(),
        FadeState::FadingOut => format!("Fading out {percent}%"),
        FadeState::Out => "Fade in".to_owned(),
        FadeState::FadingIn => format!("Fading in {percent}%"),
    };
    FadeButton {
        label,
        active: snapshot.fade.state != FadeState::In,
    }
}

/// What the notes say: why the device plays nothing, the last problem, the
/// package's warnings, and why `facts` are missing.
fn status_view(snapshot: &Snapshot, song: &SongInfo, facts: &SongFacts) -> StatusView {
    let device_problem = match &snapshot.device {
        DeviceStatus::Connected(_) => None,
        // The session closed the stream for new settings (a buffer size,
        // more outputs, another rate): the device is there and refuses them.
        DeviceStatus::Disconnected {
            device,
            reason: LossReason::Reconfigured,
            reopen_error: Some(error),
        } => Some(format!(
            "Paused: \"{device}\" refuses the new settings ({error})"
        )),
        DeviceStatus::Disconnected {
            device,
            reopen_error: Some(error),
            ..
        } => Some(format!(
            "No device: \"{device}\" (cannot open it again: {error})"
        )),
        DeviceStatus::Disconnected { device, reason, .. } => {
            Some(format!("No device: \"{device}\" ({reason})"))
        }
        DeviceStatus::PlayerLost { device } => Some(format!(
            "No device: \"{device}\" (the output kept the player; open the song again)"
        )),
    };
    StatusView {
        device_problem,
        last_error: snapshot.last_error.as_ref().map(ToString::to_string),
        warnings: song.warnings.clone(),
        facts_problem: facts.unread.clone(),
    }
}

/// `frames` at `rate` as "4.2 s".
fn seconds(frames: u64, rate: u32) -> String {
    format!("{:.1} s", frames as f64 / f64::from(rate.max(1)))
}

/// `frames` at `rate` as "mm:ss", rounded down.
fn clock(frames: u64, rate: u32) -> String {
    let seconds = frames / u64::from(rate.max(1));
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Why a song could not open, one line per problem, as the `app` API words
/// them; an invalid package lists every problem.
pub fn open_error_lines(error: &OpenError) -> Vec<String> {
    match error {
        OpenError::Load(LoadError::Invalid(problems)) => {
            problems.iter().map(ToString::to_string).collect()
        }
        other => vec![other.to_string()],
    }
}
