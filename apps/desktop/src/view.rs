//! What the Perform screen shows, mapped from a session [`Snapshot`] and the
//! open song (S6): the song map with the playhead and the queued jump, the
//! section pads with Now and Next, the transport, the fade, the jump
//! timing, loop and infinite, and the status area.

use app::{
    DeviceStatus, FadeState, JumpKind, JumpTiming, LoadError, LossReason, OpenError, PositionInfo,
    Snapshot, SongInfo, TransportState,
};

use crate::action::is_playing;

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

/// A short name for a section block on the song map: "V2" for "Verse 2".
pub fn short_label(label: &str) -> String {
    let word = first_word(label);
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
    let number = label[word.len()..].trim();
    format!("{short}{number}")
}

fn first_word(label: &str) -> &str {
    label.split(' ').next().unwrap_or(label)
}

/// One arrangement position on the song map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapBlock {
    /// Index into the arrangement.
    pub position: usize,
    /// Full label, such as "Verse 2".
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
}

/// Where and when the queued jump or loop executes (F6.5).
#[derive(Debug, Clone, PartialEq)]
pub struct JumpMarker {
    /// Where it executes, as a fraction of the arrangement.
    pub at: f32,
    /// What it is and when it executes, such as "Jump · in 4.2 s".
    pub label: String,
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
}

/// One section pad.
#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    /// Index into the arrangement; the pad queues a jump to it.
    pub position: usize,
    /// Label, such as "Verse 2".
    pub label: String,
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
    /// The current section.
    pub name: String,
    /// Time left in it, such as "6.3 s left".
    pub detail: String,
}

/// The card that says what plays next.
#[derive(Debug, Clone, PartialEq)]
pub struct NextCard {
    /// "Next", "Jump queued" or "Loop queued".
    pub heading: String,
    /// The section that plays next, or "End of song".
    pub name: String,
    /// When, such as "Executes in 4.2 s".
    pub detail: String,
    /// Whether a jump or loop is queued; the card then offers to cancel it.
    pub queued: bool,
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
}

/// The fade button.
#[derive(Debug, Clone, PartialEq)]
pub struct FadeButton {
    /// "Fade", "Fading out 40%", "Fade in" or "Fading in 60%".
    pub label: String,
    /// Whether the stems are faded or fading.
    pub active: bool,
}

/// How serious a status line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// All is well.
    Ok,
    /// Needs attention.
    Error,
}

/// The status area: the device, underruns, the last problem, and the song
/// package's warnings, in the `app` API's own words.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusView {
    /// The device and its stream, or why there is none.
    pub device: String,
    /// Whether the device plays.
    pub device_level: Level,
    /// Underruns so far, such as "Underruns: 0".
    pub underruns: String,
    /// The last problem ([`Snapshot::last_error`]), as the `app` API words it.
    pub last_error: Option<String>,
    /// Findings in the song package that do not block playback.
    pub warnings: Vec<String>,
}

/// Everything the Perform screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PerformView {
    /// Song title.
    pub title: String,
    /// Artist and arrangement.
    pub subtitle: String,
    /// The transport.
    pub transport: TransportView,
    /// Time in the arrangement and its length, such as "01:25 / 05:14".
    pub clock: String,
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
    /// The status area.
    pub status: StatusView,
}

impl PerformView {
    /// What the screen shows for `snapshot` of `song`.
    pub fn new(snapshot: &Snapshot, song: &SongInfo) -> Self {
        let timeline = Timeline::new(snapshot, song);
        let next = next_position(snapshot, song);
        let queued_target = snapshot.queued.as_ref().map(|queued| queued.target);
        let subtitle = match &song.artist {
            Some(artist) => format!("{artist} · arrangement \"{}\"", song.arrangement),
            None => format!("arrangement \"{}\"", song.arrangement),
        };
        let pads = song
            .positions
            .iter()
            .enumerate()
            .map(|(position, info)| Pad {
                position,
                label: info.label.clone(),
                kind: SectionKind::of(&info.label),
                key: char::from_digit(position as u32 + 1, 10).filter(|_| position < 9),
                is_now: position == snapshot.position,
                is_next: next == Some(position),
                is_queued: queued_target == Some(position),
            })
            .collect();
        Self {
            title: song.title.clone(),
            subtitle,
            transport: transport_view(snapshot.transport),
            clock: format!(
                "{} / {}",
                clock(timeline.elapsed, song.sample_rate),
                clock(timeline.total, song.sample_rate)
            ),
            bar_beat: format!("Bar {} · beat {}", snapshot.bar, snapshot.beat),
            map: song_map(snapshot, song, &timeline),
            now: NowCard {
                name: snapshot.section.clone(),
                detail: format!(
                    "{} left",
                    seconds(timeline.left_in_section, song.sample_rate)
                ),
            },
            next: next_card(snapshot, song, next, timeline.left_in_section),
            pads,
            jump_timing: snapshot.jump_timing,
            live_loop: snapshot.live_loop(),
            infinite: snapshot.infinite,
            fade: fade_button(snapshot),
            status: status_view(snapshot, song),
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

fn song_map(snapshot: &Snapshot, song: &SongInfo, timeline: &Timeline) -> SongMap {
    let mut start = 0;
    let blocks = song
        .positions
        .iter()
        .enumerate()
        .map(|(position, info)| {
            let block = MapBlock {
                position,
                label: info.label.clone(),
                short: short_label(&info.label),
                kind: SectionKind::of(&info.label),
                start: timeline.fraction(start),
                width: timeline.fraction(info.length_frames),
                is_now: position == snapshot.position,
                is_queued: snapshot
                    .queued
                    .as_ref()
                    .is_some_and(|q| q.target == position),
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
    }
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
    next: Option<usize>,
    left_in_section: u64,
) -> NextCard {
    let rate = song.sample_rate;
    let name = |position: usize| {
        song.positions
            .get(position)
            .map_or_else(String::new, |p| p.label.clone())
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
        };
    }
    let left = seconds(left_in_section, rate);
    let (name, detail) = match next {
        Some(position) if snapshot.infinite => {
            (name(position), format!("Repeats in {left} · infinite loop"))
        }
        Some(position) => (
            name(position),
            format!("Plays in {left} · tap a section to jump"),
        ),
        None => ("End of song".to_owned(), format!("Ends in {left}")),
    };
    NextCard {
        heading: "Next".to_owned(),
        name,
        detail,
        queued: false,
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
    TransportView {
        state: state.to_owned(),
        play_label: if playing { "Pause" } else { "Play" }.to_owned(),
        playing,
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

fn status_view(snapshot: &Snapshot, song: &SongInfo) -> StatusView {
    let (device, device_level) = match &snapshot.device {
        DeviceStatus::Connected(info) => (
            format!(
                "{} · {} Hz · {} frames · {:.1} ms",
                info.device,
                info.sample_rate,
                info.buffer_frames,
                info.latency().as_secs_f64() * 1_000.0
            ),
            Level::Ok,
        ),
        // The session closed the stream for new settings (a buffer size,
        // more outputs, another rate): the device is there and refuses them.
        DeviceStatus::Disconnected {
            device,
            reason: LossReason::Reconfigured,
            reopen_error: Some(error),
        } => (
            format!("Paused: \"{device}\" refuses the new settings ({error})"),
            Level::Error,
        ),
        DeviceStatus::Disconnected {
            device,
            reopen_error: Some(error),
            ..
        } => (
            format!("No device: \"{device}\" (cannot open it again: {error})"),
            Level::Error,
        ),
        DeviceStatus::Disconnected { device, reason, .. } => {
            (format!("No device: \"{device}\" ({reason})"), Level::Error)
        }
        DeviceStatus::PlayerLost { device } => (
            format!("No device: \"{device}\" (the output kept the player; open the song again)"),
            Level::Error,
        ),
    };
    StatusView {
        device,
        device_level,
        underruns: format!("Underruns: {}", snapshot.underruns),
        last_error: snapshot.last_error.as_ref().map(ToString::to_string),
        warnings: song.warnings.clone(),
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
