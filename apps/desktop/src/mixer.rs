//! The Mixer screen without a window (S3, F7): what its two layers show of
//! the song playing (a setlist's current item), and the `app` command each
//! control sends. The stems layer has a strip per stem (its bus, level,
//! mute and solo), the buses layer a strip per bus (the output it plays on,
//! level, mute) and the master. The song's mix is saved to its `mix.json`
//! on request; bus and master changes are global and saved to the
//! configuration folder once they settle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use app::Command;
use domain::{ArrangementMix, Bus, Destination, MAX_VOLUME_DB, Volume};

use crate::audio_settings::AudioSettings;
use crate::show::Show;

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
    /// The stem's name.
    pub name: String,
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
}

/// One bus's strip.
#[derive(Debug, Clone, PartialEq)]
pub struct BusStrip {
    /// The bus.
    pub bus: Bus,
    /// Its name.
    pub name: &'static str,
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
}

/// The master strip.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterStrip {
    /// Where it plays: "All".
    pub output: String,
    /// Its volume.
    pub volume: Volume,
    /// The readout under its fader.
    pub db: String,
    /// Where its fader stands.
    pub fader: f32,
}

/// Everything the Mixer screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerScreen {
    /// The song's title.
    pub title: String,
    /// The arrangement whose mix it is, such as "Arrangement: default".
    pub arrangement: String,
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
        let stems: Vec<StemStrip> = mixer
            .stems
            .iter()
            .enumerate()
            .map(|(index, view)| {
                let mix = mixer.mix.stem(&view.id);
                StemStrip {
                    index,
                    name: view.id.clone(),
                    bus: view.bus,
                    default_bus: mix.bus_override().is_none(),
                    volume: mix.volume,
                    db: db_text(mix.volume),
                    fader: fader_position(mix.volume),
                    mute: mix.mute,
                    solo: mix.solo,
                    silenced: view.gain == 0.0,
                }
            })
            .collect();
        let buses = Bus::ALL
            .into_iter()
            .map(|bus| {
                let level = mixer.buses.level(bus);
                BusStrip {
                    bus,
                    name: bus.name(),
                    output: output_text(mixer.routing.destination(bus)),
                    volume: level.volume,
                    db: db_text(level.volume),
                    fader: fader_position(level.volume),
                    mute: level.mute,
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
        Some(MixerScreen {
            title: song.title.clone(),
            arrangement: format!("Arrangement: {}", song.arrangement),
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
                output: "All".to_owned(),
                volume: master,
                db: db_text(master),
                fader: fader_position(master),
            },
            notices,
            bus_menu: self.bus_menu,
        })
    }
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
