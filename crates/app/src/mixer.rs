//! The mixer of a session (F7.1-F7.5): each item's arrangement mix, from
//! its song's `mix.json`, and the gains it plays with.

use std::path::{Path, PathBuf};

use domain::{
    ArrangementMix, Bus, BusSettings, MixFile, MixValidationError, Song, Stem, StemGain, StemRole,
    effective_gains,
};
use engine::live::{self, Gain, Player};
use library::read_mix;

/// One stem as the mixer shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct StemView {
    /// Stem id, as in the song package.
    pub id: String,
    /// The part it plays (F1.2).
    pub part: String,
    /// A track, the click or the guide.
    pub role: StemRole,
    /// The bus it plays through: the mix's, else the rules table's (F7.2).
    pub bus: Bus,
    /// The gain it plays at: its volume, mute and solo, its bus and the
    /// master together ([`domain::effective_gains`]).
    pub gain: f32,
}

/// What the control thread keeps of one item's mix: the stems, the
/// arrangement's mix as it plays and as its `mix.json` has it, and the
/// gains its player was last given.
#[derive(Debug, Clone)]
pub(crate) struct ItemMix {
    /// The song folder; `None` for a click-only tile.
    folder: Option<PathBuf>,
    stems: Vec<Stem>,
    arrangement: String,
    /// The song's `mix.json` as read or last saved.
    file: MixFile,
    /// The arrangement's mix as the file has it.
    saved: ArrangementMix,
    /// The arrangement's mix as it plays.
    pub(crate) mix: ArrangementMix,
    error: Option<String>,
    problems: Vec<MixValidationError>,
    /// The gain and bus of each stem, as the player has them.
    gains: Vec<StemGain>,
}

impl ItemMix {
    /// The mix of `arrangement` of `song`, from the `mix.json` of its
    /// folder `folder`: the default mix when there is none or it cannot be
    /// used.
    pub(crate) fn read(folder: &Path, song: &Song, arrangement: &str) -> Self {
        let read = read_mix(folder, song);
        let saved = read.mix.arrangement(arrangement);
        Self {
            folder: Some(folder.to_owned()),
            stems: song.stems().to_vec(),
            arrangement: arrangement.to_owned(),
            file: read.mix,
            mix: saved.clone(),
            saved,
            error: read.error.map(|error| error.to_string()),
            problems: read.problems,
            gains: Vec::new(),
        }
    }

    /// A click-only tile's: no stems, no file.
    pub(crate) fn click() -> Self {
        Self {
            folder: None,
            stems: Vec::new(),
            arrangement: String::new(),
            file: MixFile::new(),
            saved: ArrangementMix::default(),
            mix: ArrangementMix::default(),
            error: None,
            problems: Vec::new(),
            gains: Vec::new(),
        }
    }

    /// Gives `player`, not playing yet, this mix's gains with `buses`, and
    /// the click its Click & Guide gain, at once.
    pub(crate) fn prime(&mut self, player: &mut Player, buses: &BusSettings) {
        self.gains = effective_gains(&self.stems, &self.mix, buses);
        player.set_stem_gains(&self.gains);
        player.set_click_gain(buses.bus_gain(Bus::ClickGuide));
    }

    /// The commands that take the player from the gains it has to the
    /// gains of this mix with `buses`, stem by stem; [`Self::sent`] records
    /// each one the player was sent.
    pub(crate) fn changes(&self, buses: &BusSettings) -> Vec<(usize, StemGain, live::Command)> {
        let target = effective_gains(&self.stems, &self.mix, buses);
        let mut changes = Vec::new();
        for (stem, (now, then)) in self.gains.iter().zip(&target).enumerate() {
            if now.bus != then.bus {
                let command = live::Command::SetStemBus {
                    stem,
                    bus: then.bus,
                };
                changes.push((stem, *then, command));
            }
            if now.gain != then.gain {
                let gain = Gain::new(then.gain);
                changes.push((stem, *then, live::Command::SetStemGain { stem, gain }));
            }
        }
        changes
    }

    /// The player took `command`, one of [`Self::changes`], for `stem`.
    pub(crate) fn sent(&mut self, stem: usize, target: StemGain, command: live::Command) {
        if let Some(gain) = self.gains.get_mut(stem) {
            match command {
                live::Command::SetStemBus { .. } => gain.bus = target.bus,
                _ => gain.gain = target.gain,
            }
        }
    }

    /// Stems of the item.
    pub(crate) fn stems(&self) -> &[Stem] {
        &self.stems
    }

    /// Each stem as the mixer shows it.
    pub(crate) fn views(&self) -> Vec<StemView> {
        self.stems
            .iter()
            .zip(&self.gains)
            .map(|(stem, gain)| StemView {
                id: stem.id.clone(),
                part: stem.part.clone(),
                role: stem.role,
                bus: gain.bus,
                gain: gain.gain,
            })
            .collect()
    }

    /// Whether the mix as it plays differs from its `mix.json`.
    pub(crate) fn changed(&self) -> bool {
        self.mix != self.saved
    }

    /// Why `mix.json` could not be used, if so.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The rules `mix.json` breaks against the song.
    pub(crate) fn problems(&self) -> &[MixValidationError] {
        &self.problems
    }

    /// The song folder and its `mix.json` with the arrangement's mix as it
    /// plays; `None` for a click-only tile.
    pub(crate) fn to_save(&self) -> Option<(PathBuf, MixFile)> {
        let folder = self.folder.clone()?;
        let mut file = self.file.clone();
        file.set_arrangement(self.arrangement.clone(), self.mix.clone());
        Some((folder, file))
    }

    /// The mix as it plays was saved to `mix.json`.
    pub(crate) fn saved(&mut self) {
        if let Some((_, file)) = self.to_save() {
            self.file = file;
        }
        self.saved = self.mix.clone();
    }
}
