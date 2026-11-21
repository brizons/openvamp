//! What the Perform screen shows of a setlist (S4): the setlist strip with
//! every item, the current and next one and each item's transition, a
//! crossfade under way, and the setlist's problems before and during play.

use std::cmp::Ordering;

use app::{DeviceStatus, RateFallback, SetlistCommandError, SetlistInfo, SetlistSnapshot};
use domain::{SetlistItem, Transition};

use crate::catalog::{Audience, Catalog, transition_name, transition_short};

/// Where an item stands in the setlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// Before the current item.
    Played,
    /// The current item (during a crossfade, the one fading in).
    Current,
    /// After the current item.
    Upcoming,
}

/// The chip after an item that says how it leads to the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionChip {
    /// The transition the item leads to the next one with.
    pub transition: Transition,
    /// The transition the setlist holds for the item; an AutoLink or
    /// Crossfade into a song at another rate plays as Cue next.
    pub written: Transition,
    /// Short name: "Cue", "Stay", "Auto", "X-fade".
    pub short: &'static str,
    /// What it is, such as "Transition: AutoLink".
    pub label: String,
}

/// One tile of the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripItem {
    /// Index into the setlist's items; the tile selects it.
    pub index: usize,
    /// The item's number, counted from 1.
    pub number: String,
    /// Song title, or "Click".
    pub title: String,
    /// What it plays ([`Catalog::meta`]).
    pub meta: String,
    /// Whether it is a click-only tile.
    pub click: bool,
    /// Where it stands.
    pub state: ItemState,
    /// Whether it is the playable item the current one leads to.
    pub is_next: bool,
    /// The next item is not buffered yet, so its transition would wait.
    pub loading: bool,
    /// Why it cannot play, or how it plays differently (another sample
    /// rate); `None` when it plays as written.
    pub warning: Option<String>,
    /// Whether it can play at all.
    pub playable: bool,
    /// How it leads to the next item; `None` for the last item.
    pub transition: Option<TransitionChip>,
}

/// Everything the Perform screen shows of the setlist.
#[derive(Debug, Clone, PartialEq)]
pub struct SetlistView {
    /// The setlist's name.
    pub name: String,
    /// The current item's place, such as "2 of 5".
    pub position: String,
    /// Every item, in play order.
    pub items: Vec<StripItem>,
    /// The crossfade under way, such as `Crossfade 1 "Open the Gates" → 2
    /// "First Light" · 40%`.
    pub blend: Option<String>,
    /// Whether a playable item comes before the current one.
    pub has_previous: bool,
    /// Whether a playable item follows the current one.
    pub has_next: bool,
    /// The setlist's problems, one line each: items that cannot play,
    /// songs at another sample rate, the last setlist problem, a control
    /// the setlist refused, and the device held for another rate.
    pub problems: Vec<String>,
    /// Where a tile dragged along the strip would land: before the tile at
    /// this index, or after the last one at the number of items; `None`
    /// when nothing is dragged or dropping would move nothing.
    pub insertion: Option<usize>,
}

impl SetlistView {
    /// What the screen shows of the setlist `info` at `snapshot`, naming
    /// songs from `catalog`; `refused` is the last setlist control refused
    /// since the last one accepted.
    pub fn new(
        info: &SetlistInfo,
        snapshot: &SetlistSnapshot,
        catalog: &Catalog,
        refused: Option<&SetlistCommandError>,
    ) -> Self {
        let state = &snapshot.setlist;
        let title = |index: usize| {
            info.items
                .get(index)
                .map_or_else(String::new, |item| catalog.title(&item.item))
        };
        let other_rate = info.other_rate();
        let last = info.items.len().saturating_sub(1);
        let items = info
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let warning = item.problems.first().cloned().or_else(|| {
                    other_rate.contains(&index).then(|| {
                        format!(
                            "{} Hz: plays once the device reopens at its rate",
                            item.sample_rate.unwrap_or_default()
                        )
                    })
                });
                let is_next = state.next == Some(index);
                StripItem {
                    index,
                    number: (index + 1).to_string(),
                    title: catalog.title(&item.item),
                    meta: catalog.meta(&item.item, Audience::Strip),
                    click: matches!(item.item, SetlistItem::Click(_)),
                    state: match index.cmp(&state.current) {
                        Ordering::Less => ItemState::Played,
                        Ordering::Equal => ItemState::Current,
                        Ordering::Greater => ItemState::Upcoming,
                    },
                    is_next,
                    loading: is_next && !state.next_ready,
                    warning,
                    playable: item.is_playable(),
                    transition: (index < last).then(|| chip(info, state, index)),
                }
            })
            .collect();
        let blend = state.blend.map(|blend| {
            format!(
                "Crossfade {} \"{}\" → {} \"{}\" · {:.0}%",
                blend.from + 1,
                title(blend.from),
                blend.to + 1,
                title(blend.to),
                blend.progress() * 100.0
            )
        });
        Self {
            name: info.name.clone(),
            position: format!("{} of {}", state.current + 1, info.items.len()),
            items,
            blend,
            has_previous: info.items[..state.current.min(info.items.len())]
                .iter()
                .any(|item| item.is_playable()),
            has_next: state.next.is_some(),
            problems: problems(info, snapshot, catalog, refused),
            insertion: None,
        }
    }
}

/// How item `index`'s AutoLink or Crossfade plays as Cue next, as the
/// session reports it: for the current item at the rate the device runs
/// now, for the others as the setlist plays in order.
fn fallback(info: &SetlistInfo, state: &app::SetlistState, index: usize) -> Option<RateFallback> {
    if index == state.current {
        state.fallback
    } else {
        info.items.get(index)?.fallback
    }
}

/// The chip after item `index`: its transition as it plays.
fn chip(info: &SetlistInfo, state: &app::SetlistState, index: usize) -> TransitionChip {
    let written = info.items[index].item.transition();
    match fallback(info, state, index) {
        Some(fallback) => TransitionChip {
            transition: Transition::CueNext,
            written,
            short: transition_short(Transition::CueNext),
            label: format!(
                "Transition: {}, played as Cue next: item {} runs at {} Hz and the device at {} Hz",
                transition_name(written),
                fallback.next + 1,
                fallback.next_rate,
                fallback.device_rate
            ),
        },
        None => TransitionChip {
            transition: written,
            written,
            short: transition_short(written),
            label: format!("Transition: {}", transition_name(written)),
        },
    }
}

/// The setlist's problems, one line each, in this order: why the library's
/// songs could not be listed, why each item that cannot play cannot, each song at another sample rate, each link
/// that plays as Cue next because of it, the last setlist problem, the
/// control refused, and the device held for the current item's rate.
fn problems(
    info: &SetlistInfo,
    snapshot: &SetlistSnapshot,
    catalog: &Catalog,
    refused: Option<&SetlistCommandError>,
) -> Vec<String> {
    // The item's number and title, such as `3 "Missing"`.
    let name = |index: usize| {
        let title = info
            .items
            .get(index)
            .map_or_else(String::new, |item| catalog.title(&item.item));
        format!("{} \"{title}\"", index + 1)
    };
    let mut lines: Vec<String> = catalog.problem().map(str::to_owned).into_iter().collect();
    for (index, item) in info.items.iter().enumerate() {
        for problem in &item.problems {
            lines.push(format!("Item {} cannot play: {problem}", name(index)));
        }
    }
    for index in info.other_rate() {
        lines.push(format!(
            "Item {} runs at {} Hz and the device at {} Hz: it plays once the device reopens at its rate, and no AutoLink or Crossfade leads into it",
            name(index),
            info.items[index].sample_rate.unwrap_or_default(),
            info.sample_rate
        ));
    }
    for index in 0..info.items.len() {
        if let Some(fallback) = fallback(info, &snapshot.setlist, index) {
            lines.push(format!(
                "Item {} leads into item {} with Cue next, not {}: item {} runs at {} Hz and the device at {} Hz",
                name(index),
                name(fallback.next),
                transition_name(info.items[index].item.transition()),
                fallback.next + 1,
                fallback.next_rate,
                fallback.device_rate
            ));
        }
    }
    if let Some(problem) = &snapshot.setlist.problem {
        lines.push(problem.to_string());
    }
    if let Some(error) = refused {
        lines.push(format!("Refused: {error}"));
    }
    let current = snapshot.setlist.current;
    if let DeviceStatus::Disconnected {
        reopen_error: Some(error),
        ..
    } = &snapshot.song.device
        && let Some(rate) = info
            .items
            .get(current)
            .and_then(|item| item.sample_rate)
            .filter(|&rate| rate != info.sample_rate)
    {
        lines.push(format!(
            "The device is held for item {} at {rate} Hz, which it refuses ({error}); choose an item at {} Hz to play again",
            name(current),
            info.sample_rate
        ));
    }
    lines
}
