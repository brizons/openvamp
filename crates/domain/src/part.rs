//! The fixed list of instrument parts a stem can have (F1.2).
//!
//! The import gives each stem a part from its file name by the rules table
//! ([`part_for_file`]; the table is in the [`bus`] module docs),
//! and the user can pick another. Each part belongs to exactly one [`Bus`],
//! so the part a name gets also decides its bus. A stem's part is stored by
//! its id in `song.json`'s `part` string, which the rules table reads back:
//! every id but `other` names its own part.
//!
//! | Bus | Part | Id |
//! |-----|------|----|
//! | Click & Guide | Click | `click` |
//! | Click & Guide | Guide | `guide` |
//! | Drums | Drums | `drums` |
//! | Drums | Percussion | `percussion` |
//! | Drums | Loop | `loop` |
//! | Bass | Bass | `bass` |
//! | Bass | Synth bass | `synth-bass` |
//! | Keys & Pads | Keys | `keys` |
//! | Keys & Pads | Piano | `piano` |
//! | Keys & Pads | Organ | `organ` |
//! | Keys & Pads | Synth | `synth` |
//! | Keys & Pads | Pad | `pad` |
//! | Keys & Pads | Strings | `strings` |
//! | Keys & Pads | Brass | `brass` |
//! | Keys & Pads | Woodwinds | `woodwinds` |
//! | Keys & Pads | Bells | `bells` |
//! | Keys & Pads | FX | `fx` |
//! | Keys & Pads | Other | `other` |
//! | Guitars | Electric guitar | `electric-guitar` |
//! | Guitars | Acoustic guitar | `acoustic-guitar` |
//! | Guitars | Guitar | `guitar` |
//! | Vocals | Vocals | `vocals` |
//! | Vocals | Backing vocals | `backing-vocals` |
//!
//! Guitar is any guitar the name does not say is electric or acoustic, and
//! the other fretted instruments (banjo, mandolin, ukulele, dobro, requinto,
//! charango). Other is the part of a stem whose file name names no
//! instrument; it is on [`DEFAULT_BUS`].
//!
//! **Assumption (S2):** the click part gives the stem the click role and the
//! guide part the guide role ([`Part::role`]), so a recorded click or guide
//! replaces the generated one; every other part is a track.

use std::fmt;

use crate::bus::{self, Bus, DEFAULT_BUS};
use crate::package::StemRole;

/// One of the fixed instrument parts, stored by its kebab-case id
/// (`electric-guitar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Part {
    /// A recorded click.
    Click,
    /// A recorded guide: spoken cues and count-ins.
    Guide,
    /// The drum kit.
    Drums,
    /// Hand percussion, shakers, claps.
    Percussion,
    /// Loops and sequences.
    Loop,
    /// Bass guitar or upright bass.
    Bass,
    /// Synth and sub bass.
    SynthBass,
    /// Keyboards other than piano and organ: electric piano, clavinet.
    Keys,
    /// Piano.
    Piano,
    /// Organ.
    Organ,
    /// Synths and arpeggios.
    Synth,
    /// Pads, ambiences and textures.
    Pad,
    /// Strings and orchestra.
    Strings,
    /// Brass and horn section.
    Brass,
    /// Saxophones, flutes and other woodwinds.
    Woodwinds,
    /// Bells, glockenspiel and celesta.
    Bells,
    /// Effects: risers, sweeps, impacts.
    Fx,
    /// A stem whose name names no instrument.
    Other,
    /// Electric guitar.
    ElectricGuitar,
    /// Acoustic guitar.
    AcousticGuitar,
    /// A guitar not named electric or acoustic, or another fretted instrument.
    Guitar,
    /// Lead vocals.
    Vocals,
    /// Backing vocals, harmonies and choir.
    BackingVocals,
}

impl Part {
    /// Every part, in menu order: grouped by bus in [`Bus::ALL`] order.
    pub const ALL: [Part; 23] = [
        Part::Click,
        Part::Guide,
        Part::Drums,
        Part::Percussion,
        Part::Loop,
        Part::Bass,
        Part::SynthBass,
        Part::Keys,
        Part::Piano,
        Part::Organ,
        Part::Synth,
        Part::Pad,
        Part::Strings,
        Part::Brass,
        Part::Woodwinds,
        Part::Bells,
        Part::Fx,
        Part::Other,
        Part::ElectricGuitar,
        Part::AcousticGuitar,
        Part::Guitar,
        Part::Vocals,
        Part::BackingVocals,
    ];

    /// The id `song.json` stores: `click`, `synth-bass`, `electric-guitar`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Guide => "guide",
            Self::Drums => "drums",
            Self::Percussion => "percussion",
            Self::Loop => "loop",
            Self::Bass => "bass",
            Self::SynthBass => "synth-bass",
            Self::Keys => "keys",
            Self::Piano => "piano",
            Self::Organ => "organ",
            Self::Synth => "synth",
            Self::Pad => "pad",
            Self::Strings => "strings",
            Self::Brass => "brass",
            Self::Woodwinds => "woodwinds",
            Self::Bells => "bells",
            Self::Fx => "fx",
            Self::Other => "other",
            Self::ElectricGuitar => "electric-guitar",
            Self::AcousticGuitar => "acoustic-guitar",
            Self::Guitar => "guitar",
            Self::Vocals => "vocals",
            Self::BackingVocals => "backing-vocals",
        }
    }

    /// The part with this [`Part::id`], compared exactly.
    pub fn from_id(id: &str) -> Option<Part> {
        Self::ALL.into_iter().find(|part| part.id() == id)
    }

    /// The name the app shows: "Electric guitar", "FX".
    pub fn name(self) -> &'static str {
        match self {
            Self::Click => "Click",
            Self::Guide => "Guide",
            Self::Drums => "Drums",
            Self::Percussion => "Percussion",
            Self::Loop => "Loop",
            Self::Bass => "Bass",
            Self::SynthBass => "Synth bass",
            Self::Keys => "Keys",
            Self::Piano => "Piano",
            Self::Organ => "Organ",
            Self::Synth => "Synth",
            Self::Pad => "Pad",
            Self::Strings => "Strings",
            Self::Brass => "Brass",
            Self::Woodwinds => "Woodwinds",
            Self::Bells => "Bells",
            Self::Fx => "FX",
            Self::Other => "Other",
            Self::ElectricGuitar => "Electric guitar",
            Self::AcousticGuitar => "Acoustic guitar",
            Self::Guitar => "Guitar",
            Self::Vocals => "Vocals",
            Self::BackingVocals => "Backing vocals",
        }
    }

    /// The bus the part belongs to.
    pub const fn bus(self) -> Bus {
        match self {
            Self::Click | Self::Guide => Bus::ClickGuide,
            Self::Drums | Self::Percussion | Self::Loop => Bus::Drums,
            Self::Bass | Self::SynthBass => Bus::Bass,
            Self::Keys
            | Self::Piano
            | Self::Organ
            | Self::Synth
            | Self::Pad
            | Self::Strings
            | Self::Brass
            | Self::Woodwinds
            | Self::Bells
            | Self::Fx => Bus::KeysPads,
            Self::Other => DEFAULT_BUS,
            Self::ElectricGuitar | Self::AcousticGuitar | Self::Guitar => Bus::Guitars,
            Self::Vocals | Self::BackingVocals => Bus::Vocals,
        }
    }

    /// The role a stem with this part plays: the click part is a recorded
    /// click, the guide part a recorded guide, and every other part a track.
    pub fn role(self) -> StemRole {
        match self {
            Self::Click => StemRole::Click,
            Self::Guide => StemRole::Guide,
            _ => StemRole::Track,
        }
    }
}

// The menu shows the parts grouped by bus, so `Part::ALL` must keep each
// bus's parts together and the buses in `Bus::ALL` order.
const _: () = {
    let mut place = 1;
    while place < Part::ALL.len() {
        assert!(
            Part::ALL[place - 1].bus().index() <= Part::ALL[place].bus().index(),
            "Part::ALL is grouped by bus in Bus::ALL order"
        );
        place += 1;
    }
};

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The part a part or file name names by the rules table (see the
/// [`bus`] module docs), or `None` when it names no instrument.
/// Its bus is what [`bus_for_name`](crate::bus_for_name) gives.
pub fn part_for_name(name: &str) -> Option<Part> {
    bus::named_part(name)
}

/// The part the import gives a stem from its file path: the part its file
/// name (without folders or extension) names, else [`Part::Other`].
pub fn part_for_file(file: &str) -> Part {
    part_for_name(bus::file_name(file)).unwrap_or(Part::Other)
}
