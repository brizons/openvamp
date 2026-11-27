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
//! words to [`Part`]s, and each part belongs to one bus ([`Part::bus`]), so
//! the one table decides both: a name's bus ([`bus_for_name`]) is the bus of
//! its part ([`part_for_name`](crate::part_for_name)).
//!
//! | Bus | Part | English words | Spanish words |
//! |-----|------|---------------|---------------|
//! | Click & Guide | Click | click, clk, metronome | clic, metronomo |
//! | Click & Guide | Guide | guide, cue, count | guia, cuenta |
//! | Drums | Drums | drum, kit, kick, kik, snare, snr, hat, hihat, hh, tom, overhead, cymbal; generic: room | bateria, bombo, caja, redoblante, platillo |
//! | Drums | Percussion | perc, percussion, clap, snap, shaker, tamb, tambourine, conga, bongo, cajon, timbale, cowbell, timpani | percusion |
//! | Drums | Loop | generic: loop, seq, sequence, beat | generic: secuencia |
//! | Bass | Bass | bass, bassline | bajo |
//! | Bass | Synth bass | sub, synthbass, subbass | |
//! | Keys & Pads | Keys | keys, keyboard, kbd, rhodes, wurli, wurlitzer, ep, clav, mellotron, accordion | teclado |
//! | Keys & Pads | Piano | piano, pno | |
//! | Keys & Pads | Organ | organ, hammond | organo |
//! | Keys & Pads | Synth | synth; generic: arp | sintetizador, sintetizadores, sinte |
//! | Keys & Pads | Pad | pad; generic: ambient, ambience, atmos, atmosphere, texture, drone | colchon, colchones |
//! | Keys & Pads | Strings | string, orchestra, orch, violin, viola, cello, harp | cuerda, violines, chelo |
//! | Keys & Pads | Brass | brass, horn, trumpet, trombone | viento, metales, trompeta |
//! | Keys & Pads | Woodwinds | sax, saxophone, flute, woodwind | saxo, flauta |
//! | Keys & Pads | Bells | bell, glock, glockenspiel, celesta | campana |
//! | Keys & Pads | FX | generic: fx, sfx, riser, sweep, swell, noise, impact | |
//! | Guitars | Electric guitar | eg; generic: electric | generic: electrica, electrico |
//! | Guitars | Acoustic guitar | ag; generic: acoustic, nylon | generic: acustica, acustico |
//! | Guitars | Guitar | guitar, gtr, banjo, mandolin, mando, ukulele, uke, dobro; generic: steel | guitarra, requinto, charango |
//! | Vocals | Vocals | vocal, vox, voc, voice, singer; generic: spoken, speech | voz, voces |
//! | Vocals | Backing vocals | bgv, bv, choir; generic: harmony, harmonies | coro; generic: armonia |
//!
//! Words both languages share (piano, pad, sub, loop, violin) are listed once,
//! in English. Plurals the final-`s` rule below cannot reach (voces, metales,
//! violines, colchones, sintetizadores) are listed as they are.
//!
//! Two-word instruments come first: "bass guitar" is Bass; "bass synth",
//! "synth bass", "sub bass", "bajo sinte" and "bajo sintetizador" are Synth
//! bass; "bass drum" is Drums; "voz guia" (a guide vocal, noun first) is
//! Vocals; "backing vocal" is Backing vocals; "electric guitar", "electric
//! gtr" and "guitarra electrica" are Electric guitar; and "acoustic guitar",
//! "acoustic gtr", "ac gtr" and "guitarra acustica" are Acoustic guitar.
//!
//! A name is split into words at every character that is not a letter
//! (spaces, `_`, `-`, `.`, `/`, brackets, digits) and where a lowercase letter
//! meets an uppercase one (`AcGtr` is "ac gtr"); words are compared in
//! lowercase without accents (`Batería` is "bateria", `Colchón` is
//! "colchon"), and a word that is not in the table is also tried without a
//! final `s` (`Toms`, `BGVs`, `Teclados`). `EG 1`, `eg_1` and `Eg1` are all
//! "eg".
//!
//! The **last** instrument word of a name decides its part, and so its bus,
//! because file names often start with the song title ("Guide Me O Thou -
//! Piano" is Piano on Keys & Pads, "Cuenta Conmigo - Bajo" is Bass). A title
//! alone can still name an instrument ("Bajo Tu Gracia" is Bass), which a
//! part overrides. Generic words (loop, fx, ambient, acoustic...) decide only
//! when the name has no instrument word: "Gtr Loop" is Guitar, "Loop" is
//! Loop on Drums.
//!
//! A name with no word of the table goes nowhere ([`bus_for_name`] returns
//! `None`); a stem whose part and file name both go nowhere goes to
//! [`DEFAULT_BUS`], the bus of [`Part::Other`].

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::package::{Stem, StemRole};
use crate::part::Part;

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

    /// The bus's place in [`Bus::ALL`] (`Bus::ALL[bus.index()] == bus`),
    /// for whatever is kept per bus in mixer order.
    pub const fn index(self) -> usize {
        self as usize
    }
}

// `Bus::index` is the declaration order, so `Bus::ALL` must list the buses
// in that order: reordering one without the other fails to build.
const _: () = {
    let mut place = 0;
    while place < Bus::ALL.len() {
        assert!(
            Bus::ALL[place].index() == place,
            "Bus::ALL lists the buses in declaration order"
        );
        place += 1;
    }
};

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
/// [module docs](self)), or `None` when it names no instrument: the bus of
/// the part it names ([`part_for_name`](crate::part_for_name)).
pub fn bus_for_name(name: &str) -> Option<Bus> {
    named_part(name).map(Part::bus)
}

/// The part a name names by the rules table, or `None` when it names no
/// instrument. [`crate::part_for_name`] is this function.
pub(crate) fn named_part(name: &str) -> Option<Part> {
    let words: Vec<Cow<'static, str>> = words(name).into_iter().map(canonical).collect();
    let mut instrument = None;
    let mut generic = None;
    for (index, word) in words.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| words[i].as_ref());
        let rule = previous
            .and_then(|previous| phrase(previous, word))
            .map(|part| (part, Weight::Instrument))
            .or_else(|| rule(word));
        match rule {
            Some((part, Weight::Instrument)) => instrument = Some(part),
            Some((part, Weight::Generic)) => generic = Some(part),
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

/// The rules table: a lowercase word, the part it names, and its weight.
const RULES: &[(&str, Part, Weight)] = &[
    // Click & Guide.
    ("click", Part::Click, Instrument),
    ("clk", Part::Click, Instrument),
    ("metronome", Part::Click, Instrument),
    ("guide", Part::Guide, Instrument),
    ("cue", Part::Guide, Instrument),
    ("count", Part::Guide, Instrument),
    ("clic", Part::Click, Instrument),
    ("metronomo", Part::Click, Instrument),
    ("guia", Part::Guide, Instrument),
    ("cuenta", Part::Guide, Instrument),
    // Drums.
    ("drum", Part::Drums, Instrument),
    ("kit", Part::Drums, Instrument),
    ("kick", Part::Drums, Instrument),
    ("kik", Part::Drums, Instrument),
    ("snare", Part::Drums, Instrument),
    ("snr", Part::Drums, Instrument),
    ("hat", Part::Drums, Instrument),
    ("hihat", Part::Drums, Instrument),
    ("hh", Part::Drums, Instrument),
    ("tom", Part::Drums, Instrument),
    ("overhead", Part::Drums, Instrument),
    ("cymbal", Part::Drums, Instrument),
    ("clap", Part::Percussion, Instrument),
    ("snap", Part::Percussion, Instrument),
    ("shaker", Part::Percussion, Instrument),
    ("tamb", Part::Percussion, Instrument),
    ("tambourine", Part::Percussion, Instrument),
    ("perc", Part::Percussion, Instrument),
    ("percussion", Part::Percussion, Instrument),
    ("conga", Part::Percussion, Instrument),
    ("bongo", Part::Percussion, Instrument),
    ("cajon", Part::Percussion, Instrument),
    ("timbale", Part::Percussion, Instrument),
    ("cowbell", Part::Percussion, Instrument),
    ("timpani", Part::Percussion, Instrument),
    ("room", Part::Drums, Generic),
    ("loop", Part::Loop, Generic),
    ("seq", Part::Loop, Generic),
    ("sequence", Part::Loop, Generic),
    ("beat", Part::Loop, Generic),
    ("bateria", Part::Drums, Instrument),
    ("bombo", Part::Drums, Instrument),
    ("caja", Part::Drums, Instrument),
    ("redoblante", Part::Drums, Instrument),
    ("platillo", Part::Drums, Instrument),
    ("percusion", Part::Percussion, Instrument),
    ("secuencia", Part::Loop, Generic),
    // Bass.
    ("bass", Part::Bass, Instrument),
    ("sub", Part::SynthBass, Instrument),
    ("synthbass", Part::SynthBass, Instrument),
    ("subbass", Part::SynthBass, Instrument),
    ("bassline", Part::Bass, Instrument),
    ("bajo", Part::Bass, Instrument),
    // Keys & Pads.
    ("keys", Part::Keys, Instrument),
    ("keyboard", Part::Keys, Instrument),
    ("kbd", Part::Keys, Instrument),
    ("piano", Part::Piano, Instrument),
    ("pno", Part::Piano, Instrument),
    ("rhodes", Part::Keys, Instrument),
    ("wurli", Part::Keys, Instrument),
    ("wurlitzer", Part::Keys, Instrument),
    ("organ", Part::Organ, Instrument),
    ("hammond", Part::Organ, Instrument),
    ("ep", Part::Keys, Instrument),
    ("clav", Part::Keys, Instrument),
    ("synth", Part::Synth, Instrument),
    ("pad", Part::Pad, Instrument),
    ("string", Part::Strings, Instrument),
    ("orchestra", Part::Strings, Instrument),
    ("orch", Part::Strings, Instrument),
    ("violin", Part::Strings, Instrument),
    ("viola", Part::Strings, Instrument),
    ("cello", Part::Strings, Instrument),
    ("brass", Part::Brass, Instrument),
    ("horn", Part::Brass, Instrument),
    ("trumpet", Part::Brass, Instrument),
    ("trombone", Part::Brass, Instrument),
    ("sax", Part::Woodwinds, Instrument),
    ("saxophone", Part::Woodwinds, Instrument),
    ("flute", Part::Woodwinds, Instrument),
    ("woodwind", Part::Woodwinds, Instrument),
    ("harp", Part::Strings, Instrument),
    ("bell", Part::Bells, Instrument),
    ("glock", Part::Bells, Instrument),
    ("glockenspiel", Part::Bells, Instrument),
    ("celesta", Part::Bells, Instrument),
    ("mellotron", Part::Keys, Instrument),
    ("accordion", Part::Keys, Instrument),
    ("fx", Part::Fx, Generic),
    ("sfx", Part::Fx, Generic),
    ("riser", Part::Fx, Generic),
    ("sweep", Part::Fx, Generic),
    ("swell", Part::Fx, Generic),
    ("ambient", Part::Pad, Generic),
    ("ambience", Part::Pad, Generic),
    ("atmos", Part::Pad, Generic),
    ("atmosphere", Part::Pad, Generic),
    ("texture", Part::Pad, Generic),
    ("drone", Part::Pad, Generic),
    ("arp", Part::Synth, Generic),
    ("noise", Part::Fx, Generic),
    ("impact", Part::Fx, Generic),
    ("teclado", Part::Keys, Instrument),
    ("organo", Part::Organ, Instrument),
    ("sintetizador", Part::Synth, Instrument),
    ("sintetizadores", Part::Synth, Instrument),
    ("sinte", Part::Synth, Instrument),
    ("colchon", Part::Pad, Instrument),
    ("colchones", Part::Pad, Instrument),
    ("cuerda", Part::Strings, Instrument),
    ("violines", Part::Strings, Instrument),
    ("chelo", Part::Strings, Instrument),
    ("viento", Part::Brass, Instrument),
    ("metales", Part::Brass, Instrument),
    ("trompeta", Part::Brass, Instrument),
    ("saxo", Part::Woodwinds, Instrument),
    ("flauta", Part::Woodwinds, Instrument),
    ("campana", Part::Bells, Instrument),
    // Guitars.
    ("guitar", Part::Guitar, Instrument),
    ("gtr", Part::Guitar, Instrument),
    ("eg", Part::ElectricGuitar, Instrument),
    ("ag", Part::AcousticGuitar, Instrument),
    ("banjo", Part::Guitar, Instrument),
    ("mandolin", Part::Guitar, Instrument),
    ("mando", Part::Guitar, Instrument),
    ("ukulele", Part::Guitar, Instrument),
    ("uke", Part::Guitar, Instrument),
    ("dobro", Part::Guitar, Instrument),
    ("acoustic", Part::AcousticGuitar, Generic),
    ("electric", Part::ElectricGuitar, Generic),
    ("steel", Part::Guitar, Generic),
    ("nylon", Part::AcousticGuitar, Generic),
    ("guitarra", Part::Guitar, Instrument),
    ("requinto", Part::Guitar, Instrument),
    ("charango", Part::Guitar, Instrument),
    ("electrica", Part::ElectricGuitar, Generic),
    ("electrico", Part::ElectricGuitar, Generic),
    ("acustica", Part::AcousticGuitar, Generic),
    ("acustico", Part::AcousticGuitar, Generic),
    // Vocals.
    ("vocal", Part::Vocals, Instrument),
    ("vox", Part::Vocals, Instrument),
    ("voc", Part::Vocals, Instrument),
    ("bgv", Part::BackingVocals, Instrument),
    ("bv", Part::BackingVocals, Instrument),
    ("choir", Part::BackingVocals, Instrument),
    ("voice", Part::Vocals, Instrument),
    ("singer", Part::Vocals, Instrument),
    ("harmony", Part::BackingVocals, Generic),
    ("harmonies", Part::BackingVocals, Generic),
    ("spoken", Part::Vocals, Generic),
    ("speech", Part::Vocals, Generic),
    ("voz", Part::Vocals, Instrument),
    ("voces", Part::Vocals, Instrument),
    ("coro", Part::BackingVocals, Instrument),
    ("armonia", Part::BackingVocals, Generic),
];

/// Two-word instruments, checked before the second word alone.
///
/// A phrase only makes a part more precise; it never moves a name to another
/// bus. Its part is on the bus of its second word when that word names an
/// instrument, else on the bus of its first word, which then names one.
const PHRASES: &[(&str, &str, Part)] = &[
    ("bass", "guitar", Part::Bass),
    ("bass", "synth", Part::SynthBass),
    ("synth", "bass", Part::SynthBass),
    ("sub", "bass", Part::SynthBass),
    ("bass", "drum", Part::Drums),
    ("bajo", "sinte", Part::SynthBass),
    ("bajo", "sintetizador", Part::SynthBass),
    ("voz", "guia", Part::Vocals),
    ("backing", "vocal", Part::BackingVocals),
    ("electric", "guitar", Part::ElectricGuitar),
    ("electric", "gtr", Part::ElectricGuitar),
    ("guitarra", "electrica", Part::ElectricGuitar),
    ("acoustic", "guitar", Part::AcousticGuitar),
    ("acoustic", "gtr", Part::AcousticGuitar),
    ("ac", "gtr", Part::AcousticGuitar),
    ("guitarra", "acustica", Part::AcousticGuitar),
];

fn rule(word: &str) -> Option<(Part, Weight)> {
    RULES
        .iter()
        .find(|(known, _, _)| *known == word)
        .map(|&(_, part, weight)| (part, weight))
}

fn phrase(first: &str, second: &str) -> Option<Part> {
    PHRASES
        .iter()
        .find(|(a, b, _)| *a == first && *b == second)
        .map(|&(_, _, part)| part)
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
pub(crate) fn file_name(file: &str) -> &str {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}
