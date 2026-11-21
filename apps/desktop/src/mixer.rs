//! The Mixer screen without a window (S3, F7): what its two layers show of
//! the song playing (a setlist's current item), and the `app` command each
//! control sends. The header shows the song, its place in a setlist, its
//! arrangement, the time and whether it plays. The stems layer has a strip
//! per stem (named and drawn after its part, its bus, level, mute, solo
//! and meter), the buses layer a strip per bus (the output it plays on,
//! level, mute, and its left and right meters) and the master. Meters show
//! what the session reports, on a −60 to 0 dBFS scale in the canvas's
//! colours ([`meter_height`], [`meter_bands`]); the screens draw frames
//! while they move ([`needs_frames`]). The song's mix is saved to its
//! `mix.json` on request; bus and master changes are global and saved to
//! the configuration folder once they settle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use app::{Command, Meter, Snapshot};
use domain::{ArrangementMix, Bus, Destination, MAX_VOLUME_DB, SetlistItem, StemRole, Volume};

use crate::audio_settings::AudioSettings;
use crate::show::Show;
use crate::view::PerformView;

/// How long bus and master changes wait for the next one before they are
/// saved: a fader drag is saved once, when it stops.
pub const BUS_SAVE_DELAY: Duration = Duration::from_millis(500);

/// A fader cap's height, in points.
pub const FADER_CAP_HEIGHT: f32 = 14.0;

/// A fader cap's width, in points.
pub const FADER_CAP_WIDTH: f32 = 26.0;

/// The room a fader leaves above and below its travel, half a cap, so the
/// cap's centre runs from the top of the travel (+12 dB) to its bottom
/// (silent) and the cap stays inside the fader.
pub const FADER_PADDING: f32 = FADER_CAP_HEIGHT / 2.0;

/// Where the pointer at height `y` puts the cap of a fader drawn from
/// `top` and `height` points tall: 1.0 at the top of its travel, 0.0 at the
/// bottom, held there beyond either end.
pub fn fader_drag_position(top: f32, height: f32, y: f32) -> f32 {
    let travel = (height - 2.0 * FADER_PADDING).max(1.0);
    (1.0 - (y - top - FADER_PADDING) / travel).clamp(0.0, 1.0)
}

/// The fader's taper: decibels per decade of fader travel. The top is
/// [`MAX_VOLUME_DB`]; 0 dB sits a little above the middle.
const DB_PER_DECADE: f64 = 40.0;

/// Where a fader stands for `volume`, from 0.0 (silent, the bottom) to 1.0
/// (+12 dB, the top).
pub fn fader_position(volume: Volume) -> f32 {
    let db = volume.db();
    if db == f64::NEG_INFINITY {
        return 0.0;
    }
    10f64
        .powf((db - MAX_VOLUME_DB) / DB_PER_DECADE)
        .clamp(0.0, 1.0) as f32
}

/// The volume a fader at `position` (0.0 to 1.0) sets, to the tenth of a
/// decibel; the bottom is silent.
pub fn fader_volume(position: f32) -> Volume {
    let position = f64::from(position.clamp(0.0, 1.0));
    if position <= 0.0 {
        return Volume::SILENT;
    }
    let db = MAX_VOLUME_DB + DB_PER_DECADE * position.log10();
    Volume::from_db(tenths(db))
}

/// `db` to the tenth, never -0.0.
fn tenths(db: f64) -> f64 {
    let rounded = (db * 10.0).round() / 10.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// A volume as the readout under a fader shows it: "-inf", "0.0", "-6.5",
/// "+3.0".
pub fn db_text(volume: Volume) -> String {
    let db = volume.db();
    if db == f64::NEG_INFINITY {
        return "-inf".to_owned();
    }
    let db = tenths(db);
    if db > 0.0 {
        format!("+{db:.1}")
    } else {
        format!("{db:.1}")
    }
}

/// Where a destination is, as a bus strip says it: "Out 1", "Out 7/8".
pub fn output_text(destination: Destination) -> String {
    match destination {
        Destination::Mono(output) => format!("Out {output}"),
        Destination::Stereo { left, right } => format!("Out {left}/{right}"),
    }
}

/// A stem's name on its strip: its part, else its id when the part is
/// blank, in title case, every word starting with a capital and the other
/// letters as written ("synth bass" is "Synth Bass", "BGVs" stays "BGVs",
/// "hi-hat" is "Hi-Hat"). The screen names a stem whose part another stem
/// shares by its id.
pub fn stem_name(part: &str, id: &str) -> String {
    title_case(if part.trim().is_empty() { id } else { part })
}

/// `name` with underscores as spaces, single spaces between words, and
/// every word starting with a capital; the other letters stay as written.
fn title_case(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut title = String::with_capacity(words.len());
    for word in words.split_whitespace() {
        if !title.is_empty() {
            title.push(' ');
        }
        // A letter starts a word unless a letter, digit or apostrophe
        // comes before it: "hi-hat" and "(drums" capitalize too.
        let mut in_word = false;
        for c in word.chars() {
            if c.is_alphabetic() && !in_word {
                title.extend(c.to_uppercase());
            } else {
                title.push(c);
            }
            in_word = c.is_alphanumeric() || c == '\'';
        }
    }
    title
}

/// The icon (a file of `assets/icons` without `.svg`) a stem's strip
/// draws, after the design canvas's Mixer artboard: the metronome for a
/// click, the speech bubble for a guide, else the instrument its part names
/// (by the table of instrument words, English and Spanish), else the one
/// its id names, else a note.
pub fn stem_icon(role: StemRole, part: &str, id: &str) -> &'static str {
    match role {
        StemRole::Click => "metronome",
        StemRole::Guide => "speech",
        StemRole::Track => instrument_icon(part)
            .or_else(|| instrument_icon(id))
            .unwrap_or("note"),
    }
}

/// The icon a bus's strip draws, after the design canvas's bus mixer.
pub fn bus_icon(bus: Bus) -> &'static str {
    match bus {
        Bus::ClickGuide => "headphones",
        Bus::Drums => "drum",
        Bus::Bass => "bass",
        Bus::KeysPads => "piano",
        Bus::Guitars => "electric-guitar",
        Bus::Vocals => "mic",
    }
}

/// Every icon [`stem_icon`] and [`bus_icon`] can return, for the app to
/// carry each one.
pub fn strip_icons() -> impl Iterator<Item = &'static str> {
    let roles =
        [StemRole::Click, StemRole::Guide, StemRole::Track].map(|role| stem_icon(role, "", ""));
    let instruments = INSTRUMENTS.iter().flat_map(|&(_, icon, weight)| {
        let acoustic = match weight {
            Weight::Electric { acoustic } => Some(acoustic),
            Weight::Instrument | Weight::Generic => None,
        };
        std::iter::once(icon).chain(acoustic)
    });
    roles
        .into_iter()
        .chain(instruments)
        .chain(Bus::ALL.map(bus_icon))
}

/// How much a word of [`INSTRUMENTS`] says about a stem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    /// The word names an instrument: the first one decides.
    Instrument,
    /// The word names an electric instrument: the first one decides, as
    /// the `acoustic` icon when an acoustic word ([`ACOUSTIC`]) is anywhere
    /// in the name too ("Acoustic Guitar", "AcGtr").
    Electric {
        /// The icon of the instrument's acoustic kind.
        acoustic: &'static str,
    },
    /// The word is generic (loop, fx, acoustic): it decides only when no
    /// instrument is named.
    Generic,
}

/// The words that pick a stem's icon, lowercase without accents, in
/// English and Spanish, after the buses' rules table (`domain::bus`).
/// Unlike that table, the first instrument word decides: a part names the
/// instrument first ("Synth Bass" is a synth, "Bass DI" a bass).
const INSTRUMENTS: &[(&[&str], &str, Weight)] = &[
    (
        &[
            "click",
            "clk",
            "metronome",
            "count",
            "clic",
            "metronomo",
            "cuenta",
        ],
        "metronome",
        Weight::Instrument,
    ),
    (&["guide", "cue", "guia"], "speech", Weight::Instrument),
    (
        &[
            "drum",
            "kit",
            "kick",
            "kik",
            "snare",
            "snr",
            "hat",
            "hihat",
            "hh",
            "tom",
            "overhead",
            "cymbal",
            "timpani",
            "bateria",
            "bombo",
            "caja",
            "redoblante",
            "platillo",
        ],
        "drum",
        Weight::Instrument,
    ),
    (
        &[
            "perc",
            "percussion",
            "percusion",
            "shaker",
            "tamb",
            "tambourine",
            "conga",
            "bongo",
            "cajon",
            "timbale",
            "cowbell",
            "clap",
            "snap",
        ],
        "maraca",
        Weight::Instrument,
    ),
    (&["bass", "bassline", "bajo"], "bass", Weight::Instrument),
    (
        &[
            "synth",
            "synthbass",
            "sub",
            "subbass",
            "pad",
            "sinte",
            "sintetizador",
            "sintetizadores",
            "colchon",
            "colchones",
        ],
        "sine",
        Weight::Instrument,
    ),
    (
        &[
            "keys",
            "keyboard",
            "kbd",
            "piano",
            "pno",
            "rhodes",
            "wurli",
            "wurlitzer",
            "organ",
            "hammond",
            "ep",
            "clav",
            "mellotron",
            "accordion",
            "teclado",
            "organo",
        ],
        "piano",
        Weight::Instrument,
    ),
    (
        &[
            "string",
            "orchestra",
            "orch",
            "violin",
            "viola",
            "cello",
            "cuerda",
            "violines",
            "chelo",
        ],
        "violin",
        Weight::Instrument,
    ),
    (
        &["guitar", "gtr", "eg", "guitarra", "requinto"],
        "electric-guitar",
        Weight::Electric {
            acoustic: "acoustic-guitar",
        },
    ),
    (
        &[
            "ag", "banjo", "mandolin", "mando", "ukulele", "uke", "dobro", "charango",
        ],
        "acoustic-guitar",
        Weight::Instrument,
    ),
    (
        &["bgv", "bv", "choir", "coro", "voces"],
        "voices",
        Weight::Instrument,
    ),
    (
        &["vocal", "vox", "voc", "voice", "singer", "voz"],
        "mic",
        Weight::Instrument,
    ),
    (&["room"], "drum", Weight::Generic),
    (
        &["loop", "seq", "sequence", "secuencia", "beat", "arp"],
        "loop",
        Weight::Generic,
    ),
    (
        &[
            "ambient",
            "ambience",
            "atmos",
            "atmosphere",
            "texture",
            "drone",
            "swell",
        ],
        "sine",
        Weight::Generic,
    ),
    (
        &["fx", "sfx", "riser", "sweep", "impact", "noise"],
        "sparkle",
        Weight::Generic,
    ),
    (&ACOUSTIC, "acoustic-guitar", Weight::Generic),
    (
        &["electric", "electrica", "electrico"],
        "electric-guitar",
        Weight::Generic,
    ),
    (
        &["harmony", "harmonies", "armonia"],
        "voices",
        Weight::Generic,
    ),
    (&["spoken", "speech"], "speech", Weight::Generic),
];

/// Words that make an electric instrument ([`Weight::Electric`]) acoustic:
/// "Acoustic Guitar", "AcGtr".
const ACOUSTIC: [&str; 6] = ["acoustic", "ac", "acustica", "acustico", "nylon", "steel"];

/// The icon of the instrument `name` names: its first instrument word's
/// (an electric one's acoustic kind when an acoustic word is in `name`
/// too), else its first generic word's; `None` when it names none.
fn instrument_icon(name: &str) -> Option<&'static str> {
    let words = icon_words(name);
    let has_acoustic_word = words.iter().any(|word| ACOUSTIC.contains(&word.as_str()));
    let mut generic = None;
    for word in &words {
        match icon_rule(word) {
            Some((_, Weight::Electric { acoustic })) if has_acoustic_word => {
                return Some(acoustic);
            }
            Some((icon, Weight::Instrument | Weight::Electric { .. })) => return Some(icon),
            Some((icon, Weight::Generic)) => {
                generic.get_or_insert(icon);
            }
            None => {}
        }
    }
    generic
}

/// The icon and weight of `word`, also tried without a final `s`
/// ("Strings", "BGVs").
fn icon_rule(word: &str) -> Option<(&'static str, Weight)> {
    let find = |word: &str| {
        INSTRUMENTS
            .iter()
            .find(|(words, _, _)| words.contains(&word))
            .map(|&(_, icon, weight)| (icon, weight))
    };
    find(word).or_else(|| word.strip_suffix('s').and_then(find))
}

/// The lowercase words of `name` without accents: runs of letters, also
/// split where a lowercase letter meets an uppercase one ("AcGtr" is "ac
/// gtr"), as the buses' rules table reads names.
fn icon_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for c in name.chars() {
        let starts_word = !c.is_alphabetic() || (previous_lowercase && c.is_uppercase());
        if starts_word && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        if c.is_alphabetic() {
            current.extend(c.to_lowercase().map(|c| match c {
                'á' | 'à' | 'ä' | 'â' => 'a',
                'é' | 'è' | 'ë' | 'ê' => 'e',
                'í' | 'ì' | 'ï' | 'î' => 'i',
                'ó' | 'ò' | 'ö' | 'ô' => 'o',
                'ú' | 'ù' | 'ü' | 'û' => 'u',
                'ñ' => 'n',
                other => other,
            }));
        }
        previous_lowercase = c.is_lowercase();
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// The quietest level a meter shows, at the bottom of its slot: −60 dBFS.
/// The design canvas draws its meters as bare heights; this scale runs
/// linear in decibels from here to 0 dBFS at the top, so the canvas's
/// colour stops fall at −18 dBFS (where a stem's green ends,
/// [`STEM_METER_GREEN_UNTIL`]), −7.2 dBFS (yellow, [`METER_YELLOW_AT`])
/// and full scale (red).
pub const METER_BOTTOM_DB: f32 = -60.0;

/// Where green ends up a stem strip's meter, after the design canvas's
/// Mixer artboard: `#3DDC97` from the bottom to here.
pub const STEM_METER_GREEN_UNTIL: f32 = 0.70;

/// Where green ends up a bus or master strip's meter, after the canvas's
/// bus mixer.
pub const BUS_METER_GREEN_UNTIL: f32 = 0.72;

/// Where a meter turns fully yellow, from the green below it.
pub const METER_YELLOW_AT: f32 = 0.88;

/// A meter's green, as `0xRRGGBB`.
pub const METER_GREEN: u32 = 0x3DDC97;

/// A meter's yellow, as `0xRRGGBB`.
pub const METER_YELLOW: u32 = 0xE9D45B;

/// A meter's red at the top of its slot, as `0xRRGGBB`.
pub const METER_RED: u32 = 0xFF6B6B;

/// How far up its slot a meter reading `amplitude` reaches, from 0.0 (the
/// bottom) to 1.0 (the top): linear in decibels from [`METER_BOTTOM_DB`]
/// to full scale (1.0). Silence, anything quieter than the bottom, and a
/// reading that is not a level (negative or NaN) draw nothing; full scale
/// and above reach the top.
pub fn meter_height(amplitude: f32) -> f32 {
    if amplitude.is_nan() || amplitude <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * amplitude.log10();
    ((db - METER_BOTTOM_DB) / -METER_BOTTOM_DB).clamp(0.0, 1.0)
}

/// One bar of a level meter as a strip draws it, heights up its slot from
/// 0.0 to 1.0 ([`meter_height`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterBar {
    /// How far its level reaches.
    pub level: f32,
    /// Where its hold tick sits, never under the level; `None` while the
    /// hold reads silence.
    pub hold: Option<f32>,
}

impl MeterBar {
    /// A bar that reads silence: nothing drawn.
    pub const SILENT: Self = Self {
        level: 0.0,
        hold: None,
    };

    /// The bar for a meter whose `level` and `hold` read these amplitudes.
    pub fn new(level: f32, hold: f32) -> Self {
        let level = meter_height(level);
        let hold = meter_height(hold).max(level);
        Self {
            level,
            hold: (hold > 0.0).then_some(hold),
        }
    }

    /// The bars of `meter`, left then right.
    pub fn sides(meter: &Meter) -> [Self; 2] {
        [0, 1].map(|side| Self::new(meter.level[side], meter.hold[side]))
    }

    /// The one bar of a stem's meter slot for `meter`: its louder side.
    pub fn louder(meter: &Meter) -> Self {
        Self::new(
            meter.level[0].max(meter.level[1]),
            meter.hold[0].max(meter.hold[1]),
        )
    }
}

/// One stretch of a meter bar: a gradient from `bottom` at height `from`
/// up to `top` at `to`, colours as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterBand {
    /// Where it starts up the slot.
    pub from: f32,
    /// Where it ends.
    pub to: f32,
    /// Its colour at `from`.
    pub bottom: u32,
    /// Its colour at `to`.
    pub top: u32,
}

/// The colour stops up a meter's slot whose green ends at `green_until`,
/// after the design canvas's gradient: green, then yellow, red at the top.
fn meter_stops(green_until: f32) -> [(f32, u32); 4] {
    [
        (0.0, METER_GREEN),
        (green_until, METER_GREEN),
        (METER_YELLOW_AT, METER_YELLOW),
        (1.0, METER_RED),
    ]
}

/// The colour, as `0xRRGGBB`, of a meter's slot at `height` when its green
/// ends at `green_until`: the canvas's gradient, mixed in sRGB as it draws
/// it. The hold tick takes the colour where it sits.
pub fn meter_colour(height: f32, green_until: f32) -> u32 {
    let height = height.clamp(0.0, 1.0);
    let stops = meter_stops(green_until);
    for pair in stops.windows(2) {
        let ((from, bottom), (to, top)) = (pair[0], pair[1]);
        if height <= to {
            let along = if to > from {
                (height - from) / (to - from)
            } else {
                1.0
            };
            return mix(bottom, top, along.clamp(0.0, 1.0));
        }
    }
    METER_RED
}

/// `along` of the way from `from` to `to`, channel by channel.
fn mix(from: u32, to: u32, along: f32) -> u32 {
    let channel = |shift: u32| {
        let a = ((from >> shift) & 0xFF) as f32;
        let b = ((to >> shift) & 0xFF) as f32;
        ((a + (b - a) * along).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// The bands that draw a bar `height` up its slot (0.0 to 1.0), whose
/// green ends at `green_until`: the canvas's gradient across the whole
/// slot, cut at the bar's top, so a colour always marks the same level.
/// Nothing for silence.
pub fn meter_bands(height: f32, green_until: f32) -> Vec<MeterBand> {
    let height = height.clamp(0.0, 1.0);
    meter_stops(green_until)
        .windows(2)
        .filter_map(|pair| {
            let ((from, bottom), (to, _)) = (pair[0], pair[1]);
            let to = to.min(height);
            (to > from).then(|| MeterBand {
                from,
                to,
                bottom,
                top: meter_colour(to, green_until),
            })
        })
        .collect()
}

/// Whether the screens must keep drawing frames for `snapshot`, at the
/// poll rate: while its transport moves (starting, playing, or ramping out)
/// or any meter, level or hold, reads above silence. Once neither holds,
/// every meter reads 0 and stays there until the song plays again.
pub fn needs_frames(snapshot: &Snapshot) -> bool {
    use app::TransportState::{Panicking, Pausing, Playing, Starting, Stopping};
    let lit = |meter: &Meter| {
        meter
            .level
            .iter()
            .chain(&meter.hold)
            .any(|&value| value > 0.0)
    };
    let mixer = &snapshot.mixer;
    matches!(
        snapshot.transport,
        Starting | Playing | Pausing | Stopping | Panicking
    ) || mixer
        .stems
        .iter()
        .filter_map(|stem| stem.meter.as_ref())
        .any(lit)
        || mixer.bus_meters.iter().any(lit)
        || lit(&mixer.master_meter)
}

/// The two layers of the Mixer screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// A strip per bus and the master.
    Buses,
    /// A strip per stem.
    Stems,
}

/// One stem's strip.
#[derive(Debug, Clone, PartialEq)]
pub struct StemStrip {
    /// Index of the stem, in package order; its commands take it.
    pub index: usize,
    /// The stem's name ([`stem_name`]).
    pub name: String,
    /// The icon its strip draws ([`stem_icon`]).
    pub icon: &'static str,
    /// The bus it plays on.
    pub bus: Bus,
    /// Whether that is the bus the rules table gives it.
    pub default_bus: bool,
    /// Its volume.
    pub volume: Volume,
    /// The readout under its fader ([`db_text`]).
    pub db: String,
    /// Where its fader stands ([`fader_position`]).
    pub fader: f32,
    /// Whether it is muted.
    pub mute: bool,
    /// Whether it is soloed.
    pub solo: bool,
    /// Whether it plays nothing: muted, its bus or the master silent or
    /// muted, or another stem soloed.
    pub silenced: bool,
    /// Its meter, the louder of its two sides in the strip's one slot
    /// ([`MeterBar::louder`]); `None` past the 64th stem, which plays
    /// without one.
    pub meter: Option<MeterBar>,
}

/// One bus's strip.
#[derive(Debug, Clone, PartialEq)]
pub struct BusStrip {
    /// The bus.
    pub bus: Bus,
    /// Its name.
    pub name: &'static str,
    /// The icon its strip draws ([`bus_icon`]).
    pub icon: &'static str,
    /// Where it plays, from the routing in use ([`output_text`]).
    pub output: String,
    /// Its volume.
    pub volume: Volume,
    /// The readout under its fader.
    pub db: String,
    /// Where its fader stands.
    pub fader: f32,
    /// Whether it is muted.
    pub mute: bool,
    /// Its meter, left then right.
    pub meter: [MeterBar; 2],
}

/// The master strip.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterStrip {
    /// Where it plays: "Out All".
    pub output: String,
    /// Its volume.
    pub volume: Volume,
    /// The readout under its fader.
    pub db: String,
    /// Where its fader stands.
    pub fader: f32,
    /// Its meter, left then right: every bus summed.
    pub meter: [MeterBar; 2],
}

/// Everything the Mixer screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerScreen {
    /// The song's title.
    pub title: String,
    /// Its place in the setlist playing, such as "Song 2 of 5"; `None` for
    /// a song opened alone.
    pub position: Option<String>,
    /// The arrangement whose mix it is, by the name the library gives it,
    /// such as "Arrangement: Default"; by its id for a song opened alone,
    /// whose session knows no name: "Arrangement: default".
    pub arrangement: String,
    /// Time in the arrangement, such as "01:25".
    pub time: String,
    /// The arrangement's length, such as "05:14".
    pub total: String,
    /// Whether the song plays (or starts): play/pause shows pause.
    pub playing: bool,
    /// The layer on show.
    pub layer: Layer,
    /// Such as "8 stems · tap a bus to reassign".
    pub summary: String,
    /// The device's outputs, such as "8 outputs".
    pub outputs: String,
    /// A strip per stem, in package order.
    pub stems: Vec<StemStrip>,
    /// A strip per bus, in mixer order.
    pub buses: Vec<BusStrip>,
    /// The master strip.
    pub master: MasterStrip,
    /// Whether "Save mix to arrangement" does anything: the mix differs
    /// from the song's `mix.json` and the item has one.
    pub can_save: bool,
    /// Whether "Reset to song mix" changes anything.
    pub can_reset: bool,
    /// Why the mix file or the bus levels could not be read or saved, one
    /// line each.
    pub notices: Vec<String>,
    /// The stem whose bus chip shows its choices.
    pub bus_menu: Option<usize>,
}

/// What a Mixer screen control asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixerAction {
    /// Shows a layer.
    ShowLayer(Layer),
    /// A stem's fader moved to this position (0.0 to 1.0).
    StemFader {
        /// The stem.
        stem: usize,
        /// Where the fader is.
        position: f32,
    },
    /// A double click on a stem's fader: back to 0 dB.
    StemUnity(usize),
    /// A stem's M.
    ToggleStemMute(usize),
    /// A stem's S.
    ToggleStemSolo(usize),
    /// A stem's bus chip: shows its choices, or hides them.
    OpenBusMenu(usize),
    /// A stem's bus chip: another bus, or `None` for the rules table's.
    StemBus {
        /// The stem.
        stem: usize,
        /// Its bus.
        bus: Option<Bus>,
    },
    /// A bus's fader moved.
    BusFader {
        /// The bus.
        bus: Bus,
        /// Where the fader is.
        position: f32,
    },
    /// A double click on a bus's fader: back to 0 dB.
    BusUnity(Bus),
    /// A bus's M.
    ToggleBusMute(Bus),
    /// The master fader moved.
    MasterFader(f32),
    /// A double click on the master fader.
    MasterUnity,
    /// "Reset to song mix".
    Reset,
    /// "Save mix to arrangement".
    Save,
}

/// The Mixer screen's own state: the layer on show, what could not be
/// saved, and the bus changes waiting to be saved.
#[derive(Debug)]
pub struct MixerPage {
    layer: Layer,
    bus_menu: Option<usize>,
    mix_problem: Option<String>,
    bus_problem: Option<String>,
    buses_due: Option<Instant>,
}

impl Default for MixerPage {
    fn default() -> Self {
        Self::new()
    }
}

impl MixerPage {
    /// The screen on its stems layer.
    pub fn new() -> Self {
        Self {
            layer: Layer::Stems,
            bus_menu: None,
            mix_problem: None,
            bus_problem: None,
            buses_due: None,
        }
    }

    /// Does what `action` asks of the song `show` plays, at `now`: sends its
    /// mixer command (a full queue still reaches the players from the next
    /// polls), or saves the mix. Bus and master changes are saved once they
    /// settle ([`Self::tick`]).
    pub fn act(&mut self, action: MixerAction, show: &mut Show, now: Instant) {
        let mixer = &show.snapshot().mixer;
        let stem_mix = |stem: usize| {
            mixer
                .stems
                .get(stem)
                .map(|view| mixer.mix.stem(&view.id))
                .unwrap_or_default()
        };
        let command = match action {
            MixerAction::ShowLayer(layer) => {
                self.layer = layer;
                self.bus_menu = None;
                return;
            }
            MixerAction::OpenBusMenu(stem) => {
                self.bus_menu = (self.bus_menu != Some(stem)).then_some(stem);
                return;
            }
            MixerAction::Save => {
                self.save_mix(show);
                return;
            }
            MixerAction::Reset => Command::ResetMix,
            MixerAction::StemFader { stem, position } => Command::SetStemVolume {
                stem,
                volume: fader_volume(position),
            },
            MixerAction::StemUnity(stem) => Command::SetStemVolume {
                stem,
                volume: Volume::UNITY,
            },
            MixerAction::ToggleStemMute(stem) => Command::SetStemMute {
                stem,
                mute: !stem_mix(stem).mute,
            },
            MixerAction::ToggleStemSolo(stem) => Command::SetStemSolo {
                stem,
                solo: !stem_mix(stem).solo,
            },
            MixerAction::StemBus { stem, bus } => {
                self.bus_menu = None;
                Command::SetStemBus { stem, bus }
            }
            MixerAction::BusFader { bus, position } => Command::SetBusVolume {
                bus,
                volume: fader_volume(position),
            },
            MixerAction::BusUnity(bus) => Command::SetBusVolume {
                bus,
                volume: Volume::UNITY,
            },
            MixerAction::ToggleBusMute(bus) => Command::SetBusMute {
                bus,
                mute: !mixer.buses.level(bus).mute,
            },
            MixerAction::MasterFader(position) => Command::SetMasterVolume(fader_volume(position)),
            MixerAction::MasterUnity => Command::SetMasterVolume(Volume::UNITY),
        };
        // A full queue still reaches the players from the next polls.
        let _ = show.send_mixer(command);
        if matches!(
            command,
            Command::SetBusVolume { .. } | Command::SetBusMute { .. } | Command::SetMasterVolume(_)
        ) {
            self.buses_due = Some(now + BUS_SAVE_DELAY);
        }
        show.poll();
    }

    /// Saves the bus and master settings once [`BUS_SAVE_DELAY`] has passed
    /// since the last change, at `now`.
    pub fn tick(&mut self, show: &mut Show, settings: &mut AudioSettings, now: Instant) {
        if self.buses_due.is_some_and(|due| now >= due) {
            self.save_buses(show, settings);
        }
    }

    /// Whether bus or master changes wait to be saved.
    pub fn buses_waiting(&self) -> bool {
        self.buses_due.is_some()
    }

    /// Saves the bus and master settings now ([`AudioSettings::save_bus_settings`]).
    pub fn save_buses(&mut self, show: &mut Show, settings: &mut AudioSettings) {
        self.buses_due = None;
        match settings.save_bus_settings(show.bus_settings()) {
            Ok(()) => {
                show.bus_settings_saved();
                self.bus_problem = None;
            }
            Err(error) => {
                self.bus_problem = Some(format!("The bus levels could not be saved: {error}"));
            }
        }
        show.poll();
    }

    /// Writes the song's mix to its `mix.json` when it changed.
    fn save_mix(&mut self, show: &mut Show) {
        if !show.snapshot().mixer.mix_changed {
            return;
        }
        let Some((folder, file)) = show.mix_file() else {
            return;
        };
        match library::write_mix(&folder, &file) {
            Ok(()) => {
                show.mix_saved();
                self.mix_problem = None;
            }
            Err(error) => self.mix_problem = Some(format!("The mix could not be saved: {error}")),
        }
        show.poll();
    }

    /// What the screen shows of the song `show` plays; `None` with nothing
    /// loaded.
    pub fn view(&self, show: Option<&Show>) -> Option<MixerScreen> {
        let show = show?;
        let mixer = &show.snapshot().mixer;
        let song = show.song();
        // Stems that share a part ("EG 1" and "EG 2" both play "electric
        // guitar") are named by their ids, which tell them apart.
        let shares_part = |part: &str| {
            let part = part.trim().to_lowercase();
            mixer
                .stems
                .iter()
                .filter(|other| other.part.trim().to_lowercase() == part)
                .count()
                > 1
        };
        let stems: Vec<StemStrip> = mixer
            .stems
            .iter()
            .enumerate()
            .map(|(index, view)| {
                let mix = mixer.mix.stem(&view.id);
                StemStrip {
                    index,
                    name: if shares_part(&view.part) {
                        title_case(&view.id)
                    } else {
                        stem_name(&view.part, &view.id)
                    },
                    icon: stem_icon(view.role, &view.part, &view.id),
                    bus: view.bus,
                    default_bus: mix.bus_override().is_none(),
                    volume: mix.volume,
                    db: db_text(mix.volume),
                    fader: fader_position(mix.volume),
                    mute: mix.mute,
                    solo: mix.solo,
                    silenced: view.gain == 0.0,
                    meter: view.meter.as_ref().map(MeterBar::louder),
                }
            })
            .collect();
        let buses = Bus::ALL
            .into_iter()
            .zip(&mixer.bus_meters)
            .map(|(bus, meter)| {
                let level = mixer.buses.level(bus);
                BusStrip {
                    bus,
                    name: bus.name(),
                    icon: bus_icon(bus),
                    output: output_text(mixer.routing.destination(bus)),
                    volume: level.volume,
                    db: db_text(level.volume),
                    fader: fader_position(level.volume),
                    mute: level.mute,
                    meter: MeterBar::sides(meter),
                }
            })
            .collect();
        let master = mixer.buses.master_volume;
        let outputs = mixer.routing.outputs;
        let notices = mixer
            .mix_error
            .iter()
            .map(|error| format!("The song's mix could not be read: {error}"))
            .chain(
                mixer
                    .mix_problems
                    .iter()
                    .map(|problem| format!("The song's mix: {problem}")),
            )
            .chain(self.mix_problem.clone())
            .chain(self.bus_problem.clone())
            .collect();
        // The time, the length and the transport, as the Perform screen
        // shows them.
        let perform = PerformView::new(show.snapshot(), song);
        Some(MixerScreen {
            title: song.title.clone(),
            position: show
                .setlist()
                .map(|setlist| format!("Song {}", setlist.view().position)),
            arrangement: format!("Arrangement: {}", arrangement_name(show)),
            time: perform.time,
            total: perform.total,
            playing: perform.transport.playing,
            layer: self.layer,
            summary: format!(
                "{} stem{} · tap a bus to reassign",
                stems.len(),
                if stems.len() == 1 { "" } else { "s" }
            ),
            outputs: format!("{outputs} output{}", if outputs == 1 { "" } else { "s" }),
            can_save: mixer.mix_changed && show.mix_file().is_some(),
            can_reset: mixer.mix != ArrangementMix::default(),
            stems,
            buses,
            master: MasterStrip {
                output: "Out All".to_owned(),
                volume: master,
                db: db_text(master),
                fader: fader_position(master),
                meter: MeterBar::sides(&mixer.master_meter),
            },
            notices,
            bus_menu: self.bus_menu,
        })
    }
}

/// The name of the arrangement `show` plays: as the library's listing names
/// it for a setlist's song, else its id.
fn arrangement_name(show: &Show) -> String {
    let playing = &show.song().arrangement;
    show.setlist()
        .and_then(|setlist| {
            let item = setlist
                .session
                .info()
                .items
                .get(setlist.snapshot.setlist.current)?;
            let SetlistItem::Song(song) = &item.item else {
                return None;
            };
            let entry = setlist.catalog.song(&song.song)?;
            entry
                .arrangements
                .iter()
                .find(|arrangement| &arrangement.id == playing)
                .map(|arrangement| arrangement.name.clone())
        })
        .unwrap_or_else(|| playing.clone())
}

/// A song's mix not saved yet, carried into a session that opens the same
/// song again (on another device), so nothing is lost.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsavedMix {
    folder: PathBuf,
    arrangement: String,
    mix: ArrangementMix,
}

/// The mix of the song `show` plays when it differs from its `mix.json`;
/// `None` when it is saved, or the item has no mix file.
pub fn unsaved_mix(show: &Show) -> Option<UnsavedMix> {
    if !show.snapshot().mixer.mix_changed {
        return None;
    }
    let (folder, _) = show.mix_file()?;
    Some(UnsavedMix {
        folder,
        arrangement: show.song().arrangement.clone(),
        mix: show.snapshot().mixer.mix.clone(),
    })
}

/// Plays `unsaved` on the song `show` plays, when it is the same song and
/// arrangement, through the mixer commands: it stays unsaved, so "Save mix
/// to arrangement" stays offered.
pub fn restore_mix(show: &mut Show, unsaved: &UnsavedMix) {
    let same_song = show
        .mix_file()
        .is_some_and(|(folder, _)| folder == unsaved.folder);
    if !same_song || show.song().arrangement != unsaved.arrangement {
        return;
    }
    let mixer = show.snapshot().mixer.clone();
    for (stem, view) in mixer.stems.iter().enumerate() {
        let want = unsaved.mix.stem(&view.id);
        let have = mixer.mix.stem(&view.id);
        let mut commands = Vec::new();
        if want.volume != have.volume {
            commands.push(Command::SetStemVolume {
                stem,
                volume: want.volume,
            });
        }
        if want.mute != have.mute {
            commands.push(Command::SetStemMute {
                stem,
                mute: want.mute,
            });
        }
        if want.solo != have.solo {
            commands.push(Command::SetStemSolo {
                stem,
                solo: want.solo,
            });
        }
        if want.bus_override() != have.bus_override() {
            commands.push(Command::SetStemBus {
                stem,
                bus: want.bus_override(),
            });
        }
        for command in commands {
            // A full queue still reaches the players from the next polls.
            let _ = show.send_mixer(command);
        }
    }
    show.poll();
}
