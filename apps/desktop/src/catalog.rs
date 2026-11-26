//! The library's songs, as the setlist strip and the setlist editor name
//! and describe a setlist's items.

use domain::{SetlistItem, SongItem, Transition};
use library::{ArrangementEntry, SongEntry, SongListing};

/// The songs [`library::Library::list_songs`] found, in its order (by
/// title), looked up by the path a setlist item stores.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    songs: Vec<SongEntry>,
    /// Why the library's songs could not be listed, when they could not.
    problem: Option<String>,
}

impl Catalog {
    /// The catalog of `songs`, in their order.
    pub fn new(songs: Vec<SongEntry>) -> Self {
        Self {
            songs,
            problem: None,
        }
    }

    /// No songs, because they could not be listed: `problem` says why.
    /// Items then show their folder names.
    pub fn unavailable(problem: String) -> Self {
        Self {
            songs: Vec::new(),
            problem: Some(problem),
        }
    }

    /// Why the library's songs could not be listed, when they could not.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The readable songs of `listing`.
    pub fn from_listing(listing: &SongListing) -> Self {
        Self::new(listing.songs.clone())
    }

    /// Every song, in the listing's order.
    pub fn songs(&self) -> &[SongEntry] {
        &self.songs
    }

    /// The song at `path`, as a setlist item stores it.
    pub fn song(&self, path: &str) -> Option<&SongEntry> {
        self.songs.iter().find(|song| song.song == path)
    }

    /// An item's title: the song's title, its folder name without `.song`
    /// when the library does not have it, or "Click" for a click-only
    /// tile.
    pub fn title(&self, item: &SetlistItem) -> String {
        match item {
            SetlistItem::Song(song) => self.song(&song.song).map_or_else(
                || folder_name(&song.song).to_owned(),
                |entry| entry.title.clone(),
            ),
            SetlistItem::Click(_) => "Click".to_owned(),
        }
    }

    /// What an item plays, in one line, as `audience` has room for: for a
    /// song its key, tempo, arrangement ([`Audience::Strip`] only), start
    /// and end, fade-out and stem count ([`Audience::Editor`] only), such
    /// as "A · 132 BPM · Default" on the strip and "A · 132 BPM · 14
    /// stems" in the editor, or "—" when there is nothing to say (a song
    /// the library lacks, in the editor); for a click-only tile its tempo,
    /// meter and bar count ("70 BPM · 6/8 · 8 bars").
    pub fn meta(&self, item: &SetlistItem, audience: Audience) -> String {
        match item {
            SetlistItem::Song(song) => self.song_meta(song, audience),
            SetlistItem::Click(click) => {
                let meter = click.time_signature;
                let length = match click.bars {
                    Some(1) => "1 bar".to_owned(),
                    Some(bars) => format!("{bars} bars"),
                    None => "until you move on".to_owned(),
                };
                format!(
                    "{} BPM · {}/{} · {length}",
                    click.bpm, meter.numerator, meter.denominator
                )
            }
        }
    }

    fn song_meta(&self, item: &SongItem, audience: Audience) -> String {
        let mut parts = Vec::new();
        let entry = self.song(&item.song);
        let arrangement = entry.and_then(|entry| {
            parts.extend(entry.key.clone());
            parts.push(format!("{} BPM", entry.bpm));
            entry
                .arrangements
                .iter()
                .find(|arrangement| arrangement.id == item.arrangement)
        });
        let named = audience.names_arrangement();
        match arrangement {
            Some(arrangement) => {
                if named {
                    parts.push(arrangement.name.clone());
                }
                if let Some(range) = range_labels(item, arrangement) {
                    parts.push(range);
                }
            }
            None => {
                if named {
                    parts.push(format!("arrangement \"{}\"", item.arrangement));
                }
                if item.start_position.is_some() || item.end_position.is_some() {
                    parts.push(range_numbers(item));
                }
            }
        }
        if item.fade_out {
            parts.push("fade-out".to_owned());
        }
        if let Some(entry) = entry.filter(|_| audience.counts_stems()) {
            parts.push(match entry.stems {
                1 => "1 stem".to_owned(),
                stems => format!("{stems} stems"),
            });
        }
        if parts.is_empty() {
            return "—".to_owned();
        }
        parts.join(" · ")
    }
}

/// The start and end of an excerpt by section label, such as "Verse 1 –
/// Chorus"; `None` when the item plays the whole arrangement, and by
/// position number when a position is past the arrangement's end.
fn range_labels(item: &SongItem, arrangement: &ArrangementEntry) -> Option<String> {
    if item.start_position.is_none() && item.end_position.is_none() {
        return None;
    }
    let last = arrangement.positions.len().saturating_sub(1);
    let label = |position: usize| arrangement.positions.get(position).map(|p| p.label.clone());
    match (
        label(item.start_position.unwrap_or(0)),
        label(item.end_position.unwrap_or(last)),
    ) {
        (Some(start), Some(end)) => Some(format!("{start} – {end}")),
        _ => Some(range_numbers(item)),
    }
}

/// The start and end of an excerpt by position number, counted from 1:
/// "positions 2–3", "from position 2", "to position 3".
fn range_numbers(item: &SongItem) -> String {
    match (item.start_position, item.end_position) {
        (Some(start), Some(end)) => format!(
            "positions {}–{}",
            start.saturating_add(1),
            end.saturating_add(1)
        ),
        (Some(start), None) => format!("from position {}", start.saturating_add(1)),
        (None, Some(end)) => format!("to position {}", end.saturating_add(1)),
        (None, None) => String::new(),
    }
}

/// Who reads an item's meta line ([`Catalog::meta`]), which decides what it
/// has room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// The Perform screen's setlist strip: the line names the song's
    /// arrangement, since the strip shows nothing else about it, and has
    /// no room for its stem count.
    Strip,
    /// The Library's setlist editor, which shows the arrangement in a
    /// column of its own: the line ends with the song's stem count, as the
    /// design's does.
    Editor,
}

impl Audience {
    /// Whether a song's line names its arrangement.
    fn names_arrangement(self) -> bool {
        self == Self::Strip
    }

    /// Whether a song's line ends with its stem count.
    fn counts_stems(self) -> bool {
        self == Self::Editor
    }
}

/// A song folder's name without `.song`: `Hymns/Example.song` gives
/// "Example".
pub fn folder_name(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.strip_suffix(".song").unwrap_or(name)
}

/// A transition's name, as the MVP spec writes it (F5.4).
pub fn transition_name(transition: Transition) -> &'static str {
    match transition {
        Transition::CueNext => "Cue next",
        Transition::Stay => "Stay",
        Transition::AutoLink => "AutoLink",
        Transition::Crossfade => "Crossfade",
    }
}

/// A transition's short name for the strip's chip: "Cue", "Stay", "Auto",
/// "X-fade".
pub fn transition_short(transition: Transition) -> &'static str {
    match transition {
        Transition::CueNext => "Cue",
        Transition::Stay => "Stay",
        Transition::AutoLink => "Auto",
        Transition::Crossfade => "X-fade",
    }
}

/// Every transition, in the order the MVP spec lists them (F5.4).
pub const TRANSITIONS: [Transition; 4] = [
    Transition::CueNext,
    Transition::Stay,
    Transition::AutoLink,
    Transition::Crossfade,
];
