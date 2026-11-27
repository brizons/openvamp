//! The Song editor's header fields without a window, for an import
//! ([`crate::song_import`]) and for a song of the library
//! ([`crate::song_edit`]): the title, the key, the tempo and time signature
//! at the start, and the tempo or meter changes after it, each a one-line
//! field. [`HeaderFields`] holds their text and the field typed into;
//! [`HeaderFields::sync`] puts what they read as into [`HeaderValues`], and
//! [`HeaderFields::text_problems`] lists the fields that read as no value.

use domain::{MapChange, TimeSignature};

use crate::line_edit::{LineEdit, LineKey, LineOutcome};
use crate::song_import::{CHANGE_STEP_BARS, ChangeRow, FieldView, SongField};

/// What the header's fields stand for: the song's title, key, tempo and
/// time signature at the start, and its changes at bars.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HeaderValues {
    pub(crate) title: String,
    pub(crate) key: Option<String>,
    pub(crate) bpm: f64,
    pub(crate) time_signature: TimeSignature,
    pub(crate) changes: Vec<MapChange>,
}

/// The text of one tempo or meter change.
#[derive(Debug, Clone)]
struct ChangeEdit {
    bar: LineEdit,
    bpm: LineEdit,
    time: LineEdit,
}

/// The header's fields, with the one typed into.
#[derive(Debug)]
pub(crate) struct HeaderFields {
    title: LineEdit,
    key: LineEdit,
    tempo: LineEdit,
    time: LineEdit,
    changes: Vec<ChangeEdit>,
    typing: Option<SongField>,
    /// The text of the field typed into, as it was when typing started:
    /// Escape puts it back.
    before: String,
}

impl HeaderFields {
    /// The fields of `values`, its changes included, nothing typed into.
    pub(crate) fn new(values: &HeaderValues) -> Self {
        Self {
            title: LineEdit::new(values.title.clone()),
            key: LineEdit::new(values.key.clone().unwrap_or_default()),
            tempo: LineEdit::new(number(values.bpm)),
            time: LineEdit::new(meter(values.time_signature)),
            changes: values
                .changes
                .iter()
                .map(|change| ChangeEdit {
                    bar: LineEdit::new(change.bar.to_string()),
                    bpm: LineEdit::new(change.bpm.map(number).unwrap_or_default()),
                    time: LineEdit::new(change.time_signature.map(meter).unwrap_or_default()),
                })
                .collect(),
            typing: None,
            before: String::new(),
        }
    }

    /// The field the keys type into, if any.
    pub(crate) fn typing(&self) -> Option<SongField> {
        self.typing
    }

    /// Leaves the field that has the focus, keeping its text.
    pub(crate) fn stop_typing(&mut self) {
        self.typing = None;
    }

    /// Starts typing into `field`; the field typed into before keeps its
    /// text.
    pub(crate) fn start_typing(&mut self, field: SongField) {
        if self.typing == Some(field) {
            // A click in the field typed into: Escape still puts back what
            // it held before.
            return;
        }
        let Some(edit) = self.field_mut(field) else {
            return;
        };
        let text = edit.text().to_owned();
        self.before = text;
        self.typing = Some(field);
    }

    /// A key typed into the field that has the focus; whether a field
    /// took it, so what the fields read as is to be synced.
    pub(crate) fn type_key(&mut self, key: LineKey<'_>) -> bool {
        let Some(field) = self.typing else {
            return false;
        };
        let before = std::mem::take(&mut self.before);
        let Some(edit) = self.field_mut(field) else {
            self.typing = None;
            return false;
        };
        match edit.key(key) {
            LineOutcome::Editing => self.before = before,
            LineOutcome::Commit(_) => self.typing = None,
            LineOutcome::Cancel => {
                *edit = LineEdit::new(before);
                self.typing = None;
            }
        }
        true
    }

    /// Text pasted into the field that has the focus; whether a field took
    /// it.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match self.typing.and_then(|field| self.field_mut(field)) {
            Some(edit) => {
                edit.paste(text);
                true
            }
            None => false,
        }
    }

    /// "Tempo or meter change": a change [`CHANGE_STEP_BARS`] bars after
    /// the last one of `values`, at the tempo in force there.
    pub(crate) fn add_change(&mut self, values: &HeaderValues) {
        let last = values.changes.iter().max_by_key(|change| change.bar);
        let bar = last.map_or(1, |change| change.bar) + CHANGE_STEP_BARS;
        // The tempo in force where it goes: the last change's that sets
        // one, else the song's.
        let mut sorted: Vec<&MapChange> = values.changes.iter().collect();
        sorted.sort_by_key(|change| change.bar);
        let bpm = sorted
            .iter()
            .rev()
            .find_map(|change| change.bpm)
            .unwrap_or(values.bpm);
        self.changes.push(ChangeEdit {
            bar: LineEdit::new(bar.to_string()),
            bpm: LineEdit::new(number(bpm)),
            time: LineEdit::new(""),
        });
    }

    /// Removes change `index` from the fields and from `values`, so each
    /// change after it keeps what it last read as; whether there was one.
    pub(crate) fn remove_change(&mut self, index: usize, values: &mut HeaderValues) -> bool {
        if index >= self.changes.len() {
            return false;
        }
        self.changes.remove(index);
        if index < values.changes.len() {
            values.changes.remove(index);
        }
        true
    }

    /// Puts what the fields read into `values`. A text that reads as no
    /// value leaves its value as it was, and shows as a problem
    /// ([`Self::text_problems`]).
    pub(crate) fn sync(&self, values: &mut HeaderValues) {
        values.title = self.title.text().to_owned();
        let key = self.key.text().trim();
        values.key = (!key.is_empty()).then(|| key.to_owned());
        if let Some(bpm) = parse_bpm(self.tempo.text()) {
            values.bpm = bpm;
        }
        if let Some(meter) = parse_meter(self.time.text()) {
            values.time_signature = meter;
        }
        let old = std::mem::take(&mut values.changes);
        values.changes = self
            .changes
            .iter()
            .enumerate()
            .map(|(index, edit)| {
                let before = old.get(index).copied();
                MapChange {
                    bar: parse_bar(edit.bar.text())
                        .or(before.map(|change| change.bar))
                        .unwrap_or(0),
                    bpm: optional(edit.bpm.text(), parse_bpm)
                        .unwrap_or_else(|| before.and_then(|change| change.bpm)),
                    time_signature: optional(edit.time.text(), parse_meter)
                        .unwrap_or_else(|| before.and_then(|change| change.time_signature)),
                }
            })
            .collect();
    }

    /// The fields whose text reads as no value.
    pub(crate) fn text_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if parse_bpm(self.tempo.text()).is_none() {
            problems.push(not_a_tempo(self.tempo.text()));
        }
        if parse_meter(self.time.text()).is_none() {
            problems.push(not_a_meter(self.time.text()));
        }
        for edit in &self.changes {
            if parse_bar(edit.bar.text()).is_none() {
                problems.push(format!(
                    "The bar \"{}\" of a change is not a whole number, such as 17.",
                    edit.bar.text()
                ));
            }
            if optional(edit.bpm.text(), parse_bpm).is_none() {
                problems.push(not_a_tempo(edit.bpm.text()));
            }
            if optional(edit.time.text(), parse_meter).is_none() {
                problems.push(not_a_meter(edit.time.text()));
            }
        }
        problems
    }

    /// `field` as the editor shows it.
    pub(crate) fn field(&self, field: SongField) -> FieldView {
        let edit = match field {
            SongField::Title => Some(&self.title),
            SongField::Key => Some(&self.key),
            SongField::Tempo => Some(&self.tempo),
            SongField::Time => Some(&self.time),
            SongField::ChangeBar(index) => self.changes.get(index).map(|change| &change.bar),
            SongField::ChangeBpm(index) => self.changes.get(index).map(|change| &change.bpm),
            SongField::ChangeTime(index) => self.changes.get(index).map(|change| &change.time),
        };
        FieldView {
            text: edit.map(|edit| edit.text().to_owned()).unwrap_or_default(),
            editing: edit.is_some() && self.typing == Some(field),
        }
    }

    /// The changes, in the order they were added.
    pub(crate) fn change_rows(&self) -> Vec<ChangeRow> {
        (0..self.changes.len())
            .map(|index| ChangeRow {
                index,
                bar: self.field(SongField::ChangeBar(index)),
                bpm: self.field(SongField::ChangeBpm(index)),
                time: self.field(SongField::ChangeTime(index)),
            })
            .collect()
    }

    fn field_mut(&mut self, field: SongField) -> Option<&mut LineEdit> {
        Some(match field {
            SongField::Title => &mut self.title,
            SongField::Key => &mut self.key,
            SongField::Tempo => &mut self.tempo,
            SongField::Time => &mut self.time,
            SongField::ChangeBar(index) => &mut self.changes.get_mut(index)?.bar,
            SongField::ChangeBpm(index) => &mut self.changes.get_mut(index)?.bpm,
            SongField::ChangeTime(index) => &mut self.changes.get_mut(index)?.time,
        })
    }
}

fn not_a_tempo(text: &str) -> String {
    format!("The tempo \"{text}\" is not a number of beats per minute, such as 120.")
}

fn not_a_meter(text: &str) -> String {
    format!("The time signature \"{text}\" is not written like 4/4.")
}

/// A tempo typed, with a point or a comma before its decimals; `None` for
/// anything but a number.
fn parse_bpm(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|bpm| bpm.is_finite())
}

/// A time signature typed as "6/8".
fn parse_meter(text: &str) -> Option<TimeSignature> {
    let (numerator, denominator) = text.trim().split_once('/')?;
    Some(TimeSignature {
        numerator: numerator.trim().parse().ok()?,
        denominator: denominator.trim().parse().ok()?,
    })
}

fn parse_bar(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// What an optional field reads as: `Some(None)` when empty, `Some(value)`
/// when it reads as one, `None` when it reads as none.
fn optional<T>(text: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Option<T>> {
    if text.trim().is_empty() {
        Some(None)
    } else {
        parse(text).map(Some)
    }
}

/// A tempo as typed: "120", "104.5".
pub(crate) fn number(bpm: f64) -> String {
    bpm.to_string()
}

/// A time signature as typed: "4/4".
pub(crate) fn meter(meter: TimeSignature) -> String {
    format!("{}/{}", meter.numerator, meter.denominator)
}
