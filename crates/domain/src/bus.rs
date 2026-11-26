//! The six mixer buses and the rules table that sends each stem to one of
//! them (F1.2, F7.2).
//!
//! Stems go to buses automatically by part: [`bus_for_stem`] reads the stem's
//! role, then its `part`, then its file name. The user can send a stem to
//! another bus; that choice lives in the arrangement's mix (`mix.json`),
//! never here.
//!
//! # The rules table
//!
//! The table is our own, written from scratch. It maps English and Spanish
//! words to buses:
//!
//! | Bus | English words | Spanish words |
//! |-----|---------------|---------------|
//! | Click & Guide | click, clk, metronome, guide, cue, count | clic, metronomo, guia, cuenta |
//! | Drums | drum, kit, kick, kik, snare, snr, hat, hihat, hh, tom, overhead, cymbal, clap, snap, shaker, tamb, tambourine, perc, percussion, conga, bongo, cajon, timbale, cowbell, timpani; generic: room, loop, seq, sequence, beat | bateria, bombo, caja, redoblante, platillo, percusion; generic: secuencia |
//! | Bass | bass, sub, synthbass, subbass, bassline | bajo |
//! | Keys & Pads | keys, keyboard, kbd, piano, pno, rhodes, wurli, wurlitzer, organ, hammond, ep, clav, synth, pad, string, orchestra, orch, violin, viola, cello, brass, horn, trumpet, trombone, sax, saxophone, flute, woodwind, harp, bell, glock, glockenspiel, celesta, mellotron, accordion; generic: fx, sfx, riser, sweep, swell, ambient, ambience, atmos, atmosphere, texture, drone, arp, noise, impact | teclado, organo, sintetizador, sintetizadores, sinte, colchon, colchones, cuerda, violines, chelo, viento, metales, trompeta, saxo, flauta, campana |
//! | Guitars | guitar, gtr, eg, ag, banjo, mandolin, mando, ukulele, uke, dobro; generic: acoustic, electric, steel, nylon | guitarra, requinto, charango; generic: electrica, electrico, acustica, acustico |
//! | Vocals | vocal, vox, voc, bgv, bv, choir, voice, singer; generic: harmony, harmonies, spoken, speech | voz, voces, coro; generic: armonia |
//!
//! Words both languages share (piano, pad, sub, loop, violin) are listed once,
//! in English. Plurals the final-`s` rule below cannot reach (voces, metales,
//! violines, colchones, sintetizadores) are listed as they are.
//!
//! Two-word instruments come first: "bass guitar", "bass synth", "bajo sinte"
//! and "bajo sintetizador" go to Bass, "bass drum" to Drums, and "voz guia"
//! (a guide vocal, noun first) to Vocals.
//!
//! A name is split into words at every character that is not a letter
//! (spaces, `_`, `-`, `.`, `/`, brackets, digits) and where a lowercase letter
//! meets an uppercase one (`AcGtr` is "ac gtr"); words are compared in
//! lowercase without accents (`Batería` is "bateria", `Colchón` is
//! "colchon"), and a word that is not in the table is also tried without a
//! final `s` (`Toms`, `BGVs`, `Teclados`). `EG 1`, `eg_1` and `Eg1` are all
//! "eg".
//!
//! The **last** instrument word of a name decides, because file names often
//! start with the song title ("Guide Me O Thou - Piano" goes to Keys & Pads,
//! "Cuenta Conmigo - Bajo" to Bass). A title alone can still name an
//! instrument ("Bajo Tu Gracia" goes to Bass), which a part overrides.
//! Generic words (loop, fx, ambient, acoustic...) decide only when the name
//! has no instrument word: "Gtr Loop" goes to Guitars, "Loop" to Drums.
//!
//! A name with no word of the table goes nowhere ([`bus_for_name`] returns
//! `None`); a stem whose part and file name both go nowhere goes to
//! [`DEFAULT_BUS`].

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::package::{Stem, StemRole};

/// One of the six fixed buses, stored by its camelCase id (`keysPads`).
///
/// **Assumption (S1):** the six buses of the design canvas, in its mixer
/// order; the generated click and the click and guide stems go to
/// [`Bus::ClickGuide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Bus {
    /// Click & Guide: the click and the spoken guide, for the band's in-ears.
    ClickGuide,
    /// Drums, percussion and loops.
    Drums,
    /// Bass.
    Bass,
    /// Keys & Pads: keyboards, synths, pads, strings, horns and textures.
    KeysPads,
    /// Guitars.
    Guitars,
    /// Vocals.
    Vocals,
}

/// Where a stem goes when neither its part nor its file name names an
/// instrument of the rules table.
///
/// **Assumption:** Keys & Pads, the bus of the melodic and textural parts
/// that most unnamed stems are.
pub const DEFAULT_BUS: Bus = Bus::KeysPads;

impl Bus {
    /// Every bus, in mixer order.
    pub const ALL: [Bus; 6] = [
        Bus::ClickGuide,
        Bus::Drums,
        Bus::Bass,
        Bus::KeysPads,
        Bus::Guitars,
        Bus::Vocals,
    ];

    /// The id files store: `clickGuide`, `drums`, `bass`, `keysPads`,
    /// `guitars`, `vocals`.
    pub fn id(self) -> &'static str {
        match self {
            Self::ClickGuide => "clickGuide",
            Self::Drums => "drums",
            Self::Bass => "bass",
            Self::KeysPads => "keysPads",
            Self::Guitars => "guitars",
            Self::Vocals => "vocals",
        }
    }

    /// The name the mixer shows: "Click & Guide", "Keys & Pads".
    pub fn name(self) -> &'static str {
        match self {
            Self::ClickGuide => "Click & Guide",
            Self::Drums => "Drums",
            Self::Bass => "Bass",
            Self::KeysPads => "Keys & Pads",
            Self::Guitars => "Guitars",
            Self::Vocals => "Vocals",
        }
    }

    /// The bus with this [`Bus::id`], compared exactly.
    pub fn from_id(id: &str) -> Option<Bus> {
        Self::ALL.into_iter().find(|bus| bus.id() == id)
    }
}

impl fmt::Display for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The bus a stem goes to by the rules table: click and guide stems always go
/// to [`Bus::ClickGuide`]; any other stem goes where its `part` says, else
/// where its file name says (without folders or extension), else to
/// [`DEFAULT_BUS`].
pub fn bus_for_stem(stem: &Stem) -> Bus {
    if matches!(stem.role, StemRole::Click | StemRole::Guide) {
        return Bus::ClickGuide;
    }
    bus_for_name(&stem.part)
        .or_else(|| bus_for_name(file_name(&stem.file)))
        .unwrap_or(DEFAULT_BUS)
}

/// The bus a part or file name names by the rules table (see the
/// [module docs](self)), or `None` when it names no instrument.
pub fn bus_for_name(name: &str) -> Option<Bus> {
    let words: Vec<Cow<'static, str>> = words(name).into_iter().map(canonical).collect();
    let mut instrument = None;
    let mut generic = None;
    for (index, word) in words.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| words[i].as_ref());
        let rule = previous
            .and_then(|previous| phrase(previous, word))
            .map(|bus| (bus, Weight::Instrument))
            .or_else(|| rule(word));
        match rule {
            Some((bus, Weight::Instrument)) => instrument = Some(bus),
            Some((bus, Weight::Generic)) => generic = Some(bus),
            None => {}
        }
    }
    instrument.or(generic)
}

/// How much a word of the table says about a stem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    /// The word names an instrument.
    Instrument,
    /// The word is generic (loop, fx, acoustic): it decides only when no
    /// instrument is named.
    Generic,
}

use Weight::{Generic, Instrument};

/// The rules table: a lowercase word and where it sends a stem.
const RULES: &[(&str, Bus, Weight)] = &[
    // Click & Guide.
    ("click", Bus::ClickGuide, Instrument),
    ("clk", Bus::ClickGuide, Instrument),
    ("metronome", Bus::ClickGuide, Instrument),
    ("guide", Bus::ClickGuide, Instrument),
    ("cue", Bus::ClickGuide, Instrument),
    ("count", Bus::ClickGuide, Instrument),
    ("clic", Bus::ClickGuide, Instrument),
    ("metronomo", Bus::ClickGuide, Instrument),
    ("guia", Bus::ClickGuide, Instrument),
    ("cuenta", Bus::ClickGuide, Instrument),
    // Drums.
    ("drum", Bus::Drums, Instrument),
    ("kit", Bus::Drums, Instrument),
    ("kick", Bus::Drums, Instrument),
    ("kik", Bus::Drums, Instrument),
    ("snare", Bus::Drums, Instrument),
    ("snr", Bus::Drums, Instrument),
    ("hat", Bus::Drums, Instrument),
    ("hihat", Bus::Drums, Instrument),
    ("hh", Bus::Drums, Instrument),
    ("tom", Bus::Drums, Instrument),
    ("overhead", Bus::Drums, Instrument),
    ("cymbal", Bus::Drums, Instrument),
    ("clap", Bus::Drums, Instrument),
    ("snap", Bus::Drums, Instrument),
    ("shaker", Bus::Drums, Instrument),
    ("tamb", Bus::Drums, Instrument),
    ("tambourine", Bus::Drums, Instrument),
    ("perc", Bus::Drums, Instrument),
    ("percussion", Bus::Drums, Instrument),
    ("conga", Bus::Drums, Instrument),
    ("bongo", Bus::Drums, Instrument),
    ("cajon", Bus::Drums, Instrument),
    ("timbale", Bus::Drums, Instrument),
    ("cowbell", Bus::Drums, Instrument),
    ("timpani", Bus::Drums, Instrument),
    ("room", Bus::Drums, Generic),
    ("loop", Bus::Drums, Generic),
    ("seq", Bus::Drums, Generic),
    ("sequence", Bus::Drums, Generic),
    ("beat", Bus::Drums, Generic),
    ("bateria", Bus::Drums, Instrument),
    ("bombo", Bus::Drums, Instrument),
    ("caja", Bus::Drums, Instrument),
    ("redoblante", Bus::Drums, Instrument),
    ("platillo", Bus::Drums, Instrument),
    ("percusion", Bus::Drums, Instrument),
    ("secuencia", Bus::Drums, Generic),
    // Bass.
    ("bass", Bus::Bass, Instrument),
    ("sub", Bus::Bass, Instrument),
    ("synthbass", Bus::Bass, Instrument),
    ("subbass", Bus::Bass, Instrument),
    ("bassline", Bus::Bass, Instrument),
    ("bajo", Bus::Bass, Instrument),
    // Keys & Pads.
    ("keys", Bus::KeysPads, Instrument),
    ("keyboard", Bus::KeysPads, Instrument),
    ("kbd", Bus::KeysPads, Instrument),
    ("piano", Bus::KeysPads, Instrument),
    ("pno", Bus::KeysPads, Instrument),
    ("rhodes", Bus::KeysPads, Instrument),
    ("wurli", Bus::KeysPads, Instrument),
    ("wurlitzer", Bus::KeysPads, Instrument),
    ("organ", Bus::KeysPads, Instrument),
    ("hammond", Bus::KeysPads, Instrument),
    ("ep", Bus::KeysPads, Instrument),
    ("clav", Bus::KeysPads, Instrument),
    ("synth", Bus::KeysPads, Instrument),
    ("pad", Bus::KeysPads, Instrument),
    ("string", Bus::KeysPads, Instrument),
    ("orchestra", Bus::KeysPads, Instrument),
    ("orch", Bus::KeysPads, Instrument),
    ("violin", Bus::KeysPads, Instrument),
    ("viola", Bus::KeysPads, Instrument),
    ("cello", Bus::KeysPads, Instrument),
    ("brass", Bus::KeysPads, Instrument),
    ("horn", Bus::KeysPads, Instrument),
    ("trumpet", Bus::KeysPads, Instrument),
    ("trombone", Bus::KeysPads, Instrument),
    ("sax", Bus::KeysPads, Instrument),
    ("saxophone", Bus::KeysPads, Instrument),
    ("flute", Bus::KeysPads, Instrument),
    ("woodwind", Bus::KeysPads, Instrument),
    ("harp", Bus::KeysPads, Instrument),
    ("bell", Bus::KeysPads, Instrument),
    ("glock", Bus::KeysPads, Instrument),
    ("glockenspiel", Bus::KeysPads, Instrument),
    ("celesta", Bus::KeysPads, Instrument),
    ("mellotron", Bus::KeysPads, Instrument),
    ("accordion", Bus::KeysPads, Instrument),
    ("fx", Bus::KeysPads, Generic),
    ("sfx", Bus::KeysPads, Generic),
    ("riser", Bus::KeysPads, Generic),
    ("sweep", Bus::KeysPads, Generic),
    ("swell", Bus::KeysPads, Generic),
    ("ambient", Bus::KeysPads, Generic),
    ("ambience", Bus::KeysPads, Generic),
    ("atmos", Bus::KeysPads, Generic),
    ("atmosphere", Bus::KeysPads, Generic),
    ("texture", Bus::KeysPads, Generic),
    ("drone", Bus::KeysPads, Generic),
    ("arp", Bus::KeysPads, Generic),
    ("noise", Bus::KeysPads, Generic),
    ("impact", Bus::KeysPads, Generic),
    ("teclado", Bus::KeysPads, Instrument),
    ("organo", Bus::KeysPads, Instrument),
    ("sintetizador", Bus::KeysPads, Instrument),
    ("sintetizadores", Bus::KeysPads, Instrument),
    ("sinte", Bus::KeysPads, Instrument),
    ("colchon", Bus::KeysPads, Instrument),
    ("colchones", Bus::KeysPads, Instrument),
    ("cuerda", Bus::KeysPads, Instrument),
    ("violines", Bus::KeysPads, Instrument),
    ("chelo", Bus::KeysPads, Instrument),
    ("viento", Bus::KeysPads, Instrument),
    ("metales", Bus::KeysPads, Instrument),
    ("trompeta", Bus::KeysPads, Instrument),
    ("saxo", Bus::KeysPads, Instrument),
    ("flauta", Bus::KeysPads, Instrument),
    ("campana", Bus::KeysPads, Instrument),
    // Guitars.
    ("guitar", Bus::Guitars, Instrument),
    ("gtr", Bus::Guitars, Instrument),
    ("eg", Bus::Guitars, Instrument),
    ("ag", Bus::Guitars, Instrument),
    ("banjo", Bus::Guitars, Instrument),
    ("mandolin", Bus::Guitars, Instrument),
    ("mando", Bus::Guitars, Instrument),
    ("ukulele", Bus::Guitars, Instrument),
    ("uke", Bus::Guitars, Instrument),
    ("dobro", Bus::Guitars, Instrument),
    ("acoustic", Bus::Guitars, Generic),
    ("electric", Bus::Guitars, Generic),
    ("steel", Bus::Guitars, Generic),
    ("nylon", Bus::Guitars, Generic),
    ("guitarra", Bus::Guitars, Instrument),
    ("requinto", Bus::Guitars, Instrument),
    ("charango", Bus::Guitars, Instrument),
    ("electrica", Bus::Guitars, Generic),
    ("electrico", Bus::Guitars, Generic),
    ("acustica", Bus::Guitars, Generic),
    ("acustico", Bus::Guitars, Generic),
    // Vocals.
    ("vocal", Bus::Vocals, Instrument),
    ("vox", Bus::Vocals, Instrument),
    ("voc", Bus::Vocals, Instrument),
    ("bgv", Bus::Vocals, Instrument),
    ("bv", Bus::Vocals, Instrument),
    ("choir", Bus::Vocals, Instrument),
    ("voice", Bus::Vocals, Instrument),
    ("singer", Bus::Vocals, Instrument),
    ("harmony", Bus::Vocals, Generic),
    ("harmonies", Bus::Vocals, Generic),
    ("spoken", Bus::Vocals, Generic),
    ("speech", Bus::Vocals, Generic),
    ("voz", Bus::Vocals, Instrument),
    ("voces", Bus::Vocals, Instrument),
    ("coro", Bus::Vocals, Instrument),
    ("armonia", Bus::Vocals, Generic),
];

/// Two-word instruments, checked before the second word alone.
const PHRASES: &[(&str, &str, Bus)] = &[
    ("bass", "guitar", Bus::Bass),
    ("bass", "synth", Bus::Bass),
    ("bass", "drum", Bus::Drums),
    ("bajo", "sinte", Bus::Bass),
    ("bajo", "sintetizador", Bus::Bass),
    ("voz", "guia", Bus::Vocals),
];

fn rule(word: &str) -> Option<(Bus, Weight)> {
    RULES
        .iter()
        .find(|(known, _, _)| *known == word)
        .map(|&(_, bus, weight)| (bus, weight))
}

fn phrase(first: &str, second: &str) -> Option<Bus> {
    PHRASES
        .iter()
        .find(|(a, b, _)| *a == first && *b == second)
        .map(|&(_, _, bus)| bus)
}

/// The word as the table knows it: unchanged when the table has it or when
/// it does not end in `s`, else without its final `s` when the table has that.
fn canonical(word: String) -> Cow<'static, str> {
    if rule(&word).is_none()
        && let Some(singular) = word.strip_suffix('s')
        && let Some(&(known, _, _)) = RULES.iter().find(|(known, _, _)| *known == singular)
    {
        return Cow::Borrowed(known);
    }
    Cow::Owned(word)
}

/// The lowercase words of `name`: runs of letters, also split where a
/// lowercase letter meets an uppercase one.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for c in name.chars() {
        if !c.is_alphabetic() {
            push_word(&mut words, &mut current);
            previous_lowercase = false;
            continue;
        }
        if previous_lowercase && c.is_uppercase() {
            push_word(&mut words, &mut current);
        }
        current.extend(c.to_lowercase().map(without_accent));
        previous_lowercase = c.is_lowercase();
    }
    push_word(&mut words, &mut current);
    words
}

fn push_word(words: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        words.push(std::mem::take(current));
    }
}

/// A lowercase letter without its accent: `á` is `a`, `ñ` is `n`.
fn without_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        _ => c,
    }
}

/// The file's own name, without the folders above it or its extension.
fn file_name(file: &str) -> &str {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}
