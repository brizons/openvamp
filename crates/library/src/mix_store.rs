//! A song's `mix.json` (F7.5, L2): the mix of each of its arrangements,
//! beside `song.json` in the song folder, which is never written.
//!
//! The format is [`domain::MixFile`]'s; the reading and writing rules are the
//! [settings files'](crate::SettingsError): a song without `mix.json`, or
//! with one that cannot be used, plays with the default mix, and a usable
//! file that breaks the song's rules (an arrangement or stem the song lacks)
//! is still used, with its problems reported.

use std::fs;
use std::path::Path;

use domain::{FORMAT_VERSION, MixFile, MixValidationError, SectionMarker, Song, SongPackage};

use crate::settings::{SettingsError, read_settings, to_json, write_settings};

/// Name of the mix file inside a song folder.
pub const MIX_FILE: &str = "mix.json";

/// A song's mix as read from its folder.
#[derive(Debug)]
pub struct SongMix {
    /// The mix to play: the file's, or [`MixFile::new`] (every stem at its
    /// default) when there is no usable file.
    pub mix: MixFile,
    /// Why `mix.json` could not be used, so the song plays with the default
    /// mix: unreadable, not a regular file, outside the song folder, too
    /// large, malformed, or a newer `formatVersion`. The file is left as it
    /// is. `None` when the file was used or there is none.
    pub error: Option<SettingsError>,
    /// The rules the file breaks against the song, every one at once; the
    /// mix is still used, and entries the song lacks are ignored when it
    /// plays.
    pub problems: Vec<MixValidationError>,
}

/// Reads the `mix.json` of the song folder `dir`, which holds `song`. It
/// never fails: what goes wrong is in [`SongMix::error`] and
/// [`SongMix::problems`], and the song still plays.
pub fn read_mix(dir: &Path, song: &Song) -> SongMix {
    let folder = match fs::canonicalize(dir) {
        Ok(folder) => folder,
        Err(source) => {
            return SongMix {
                mix: MixFile::new(),
                error: Some(SettingsError::Folder {
                    path: dir.to_owned(),
                    source,
                }),
                problems: Vec::new(),
            };
        }
    };
    let read = read_settings(&folder, MIX_FILE, MixFile::from_json);
    let problems = match &read.error {
        None => read
            .value
            .validate(&package_of(song))
            .err()
            .unwrap_or_default(),
        Some(_) => Vec::new(),
    };
    SongMix {
        mix: read.value,
        error: read.error,
        problems,
    }
}

/// Writes `mix` as the `mix.json` of the song folder `dir`, atomically, and
/// never touches `song.json`. A `mix.json` that cannot be read or parsed is
/// replaced; one with a newer `formatVersion` is not
/// ([`SettingsError::NewerVersion`]). The song folder must exist.
pub fn write_mix(dir: &Path, mix: &MixFile) -> Result<(), SettingsError> {
    let folder = fs::canonicalize(dir).map_err(|source| SettingsError::Folder {
        path: dir.to_owned(),
        source,
    })?;
    let json = to_json(&folder, MIX_FILE, mix.to_json())?;
    write_settings(&folder, MIX_FILE, &json, MixFile::from_json)
}

/// The package `song` was validated from, for [`MixFile::validate`].
fn package_of(song: &Song) -> SongPackage {
    SongPackage {
        format_version: FORMAT_VERSION,
        title: song.title().to_owned(),
        artist: song.artist().map(str::to_owned),
        sample_rate: song.sample_rate(),
        key: song.key().map(str::to_owned),
        tempo_map: song.tempo_map().to_vec(),
        time_signature_map: song.time_signature_map().to_vec(),
        stems: song.stems().to_vec(),
        sections: song
            .sections()
            .iter()
            .map(|section| SectionMarker {
                id: section.id.clone(),
                section_type: section.section_type,
                number: section.number,
                start: section.start,
            })
            .collect(),
        arrangements: song.arrangements().to_vec(),
    }
}
