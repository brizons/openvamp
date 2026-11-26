//! The app's configuration folder (F7.5, F8.3, L2): the global bus settings
//! in [`BUS_SETTINGS_FILE`] and the routing of every device in
//! [`ROUTING_FILE`], written and read with the rules of the
//! [settings files](crate::SettingsError).
//!
//! The folder is the platform's place for app settings
//! ([`config_folder`]):
//!
//! | Platform | Folder |
//! |----------|--------|
//! | macOS | `~/Library/Application Support/OpenVamp` |
//! | Windows | `%APPDATA%\OpenVamp` |
//! | Linux and other Unix-like systems | `$XDG_CONFIG_HOME/openvamp`, or `~/.config/openvamp` |
//!
//! The phone and tablet shells, and tests, give the folder themselves
//! ([`ConfigStore::new`]). The folder is created on the first write; until
//! then, and whenever a file is missing, the settings are the defaults.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use domain::{BusSettings, BusSettingsError, RoutingFile, RoutingValidationError};

use crate::settings::{Read, SettingsError, read_settings, to_json, write_settings};

/// Name of the global bus settings file in the configuration folder.
pub const BUS_SETTINGS_FILE: &str = "bus-settings.json";

/// Name of the per-device routing file in the configuration folder.
pub const ROUTING_FILE: &str = "routing.json";

/// Which platform's convention [`config_folder`] follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS: `Application Support` in the home folder.
    MacOs,
    /// Windows: the roaming application data folder.
    Windows,
    /// Linux and other Unix-like systems: the XDG Base Directory
    /// convention.
    Linux,
}

impl Platform {
    /// The platform this program was built for; any system other than macOS
    /// and Windows follows [`Platform::Linux`].
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// The configuration folder on `platform`, from the environment variables
/// `var` returns (pass `|name| std::env::var_os(name)` for the real ones):
///
/// - macOS: `$HOME/Library/Application Support/OpenVamp`.
/// - Windows: `%APPDATA%\OpenVamp`.
/// - Linux: `$XDG_CONFIG_HOME/openvamp`, or `$HOME/.config/openvamp` when
///   `XDG_CONFIG_HOME` is unset, empty or relative, as the XDG Base
///   Directory specification says.
///
/// `None` when the variable it needs is unset, empty or relative, so the
/// settings never land below the current folder. Whether a path is absolute
/// follows `platform`, not the system this runs on: `/` starts one on macOS
/// and Linux, and a drive (`C:\`) or a share (`\\server`) on Windows. Nothing
/// on disk is looked at.
pub fn config_folder(
    platform: Platform,
    var: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let absolute = |name: &str| {
        var(name)
            .filter(|value| is_absolute_on(platform, value))
            .map(PathBuf::from)
    };
    match platform {
        Platform::MacOs => absolute("HOME").map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("OpenVamp")
        }),
        Platform::Windows => absolute("APPDATA").map(|appdata| appdata.join("OpenVamp")),
        Platform::Linux => absolute("XDG_CONFIG_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".config")))
            .map(|folder| folder.join("openvamp")),
    }
}

/// Whether `path` is absolute on `platform`: it starts with `/` on macOS and
/// Linux, and with a drive and a separator (`C:\`, `c:/`) or two separators
/// (a share, `\\server`) on Windows.
fn is_absolute_on(platform: Platform, path: &OsStr) -> bool {
    let path = path.to_string_lossy();
    let separator = |c: char| c == '\\' || c == '/';
    match platform {
        Platform::MacOs | Platform::Linux => path.starts_with('/'),
        Platform::Windows => {
            let mut chars = path.chars();
            match (chars.next(), chars.next(), chars.next()) {
                (Some(drive), Some(':'), Some(slash)) => {
                    drive.is_ascii_alphabetic() && separator(slash)
                }
                (Some(first), Some(second), _) => separator(first) && separator(second),
                _ => false,
            }
        }
    }
}

/// The global bus settings as read.
#[derive(Debug)]
pub struct LoadedBusSettings {
    /// The settings to play with: the file's, or [`BusSettings::new`] (every
    /// bus and the master at unity) when there is no usable file.
    pub settings: BusSettings,
    /// Why [`BUS_SETTINGS_FILE`] could not be used, so the defaults are used:
    /// unreadable, not a regular file, outside the folder, too large,
    /// malformed, or a newer `formatVersion`. The file is left as it is until
    /// the settings are saved. `None` when the file was used or there is none.
    pub error: Option<SettingsError>,
    /// The rules the file breaks, every one at once; the settings are still
    /// used, and gains are capped when they play.
    pub problems: Vec<BusSettingsError>,
}

/// The per-device routing as read.
#[derive(Debug)]
pub struct LoadedRouting {
    /// The routing of every device: the file's, or an empty
    /// [`RoutingFile`] (every device plays its preset) when there is no
    /// usable file.
    pub routing: RoutingFile,
    /// Why [`ROUTING_FILE`] could not be used, as in
    /// [`LoadedBusSettings::error`].
    pub error: Option<SettingsError>,
    /// The rules the file breaks, every one at once; the routing is still
    /// used, and [`RoutingFile::routing`] fits each device's to its outputs.
    pub problems: Vec<RoutingValidationError>,
}

/// The app's configuration folder, holding the bus settings and the routing.
///
/// Loading never fails and never writes: what goes wrong is reported with
/// the defaults. Saving creates the folder when needed and writes one file
/// atomically, leaving the other alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigStore {
    folder: PathBuf,
}

impl ConfigStore {
    /// The store in `folder`, which need not exist yet.
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
        }
    }

    /// The store in this platform's configuration folder
    /// ([`config_folder`] with the real environment), or `None` when the
    /// environment does not say where it is.
    pub fn from_environment() -> Option<Self> {
        config_folder(Platform::current(), |name| std::env::var_os(name)).map(Self::new)
    }

    /// The configuration folder, as given.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Reads the global bus settings.
    pub fn load_bus_settings(&self) -> LoadedBusSettings {
        let Read { value, error } = self.read(BUS_SETTINGS_FILE, BusSettings::from_json);
        let problems = match error {
            None => value.validate().err().unwrap_or_default(),
            Some(_) => Vec::new(),
        };
        LoadedBusSettings {
            settings: value,
            error,
            problems,
        }
    }

    /// Saves the global bus settings, unless the file on disk has a newer
    /// `formatVersion` ([`SettingsError::NewerVersion`]).
    pub fn save_bus_settings(&self, settings: &BusSettings) -> Result<(), SettingsError> {
        self.save(
            BUS_SETTINGS_FILE,
            settings.to_json(),
            BusSettings::from_json,
        )
    }

    /// Reads the routing of every device.
    pub fn load_routing(&self) -> LoadedRouting {
        let Read { value, error } = self.read(ROUTING_FILE, RoutingFile::from_json);
        let problems = match error {
            None => value.validate().err().unwrap_or_default(),
            Some(_) => Vec::new(),
        };
        LoadedRouting {
            routing: value,
            error,
            problems,
        }
    }

    /// Saves the routing of every device, unless the file on disk has a
    /// newer `formatVersion` ([`SettingsError::NewerVersion`]).
    pub fn save_routing(&self, routing: &RoutingFile) -> Result<(), SettingsError> {
        self.save(ROUTING_FILE, routing.to_json(), RoutingFile::from_json)
    }

    /// Writes `json` as `file`. The folder is created only once the write is
    /// going ahead: after the settings became JSON, and only when it is
    /// missing, so no file there can need its version checked.
    fn save<T>(
        &self,
        file: &str,
        json: Result<String, domain::MixerFileError>,
        parse: impl FnOnce(&str) -> Result<T, domain::MixerFileError>,
    ) -> Result<(), SettingsError> {
        let json = to_json(&self.folder, file, json)?;
        let folder = self.folder_for_writing()?;
        write_settings(&folder, file, &json, parse)
    }

    /// Reads `file` from the folder; a missing folder means the defaults.
    fn read<T: Default>(
        &self,
        file: &str,
        parse: impl FnOnce(&str) -> Result<T, domain::MixerFileError>,
    ) -> Read<T> {
        match fs::canonicalize(&self.folder) {
            // Reading through a file fails as "not found" on some systems
            // (Windows), so a folder that is a file is reported here.
            Ok(folder) if !folder.is_dir() => Read {
                value: T::default(),
                error: Some(SettingsError::Folder {
                    path: self.folder.clone(),
                    source: io::Error::from(io::ErrorKind::NotADirectory),
                }),
            },
            Ok(folder) => read_settings(&folder, file, parse),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Read {
                value: T::default(),
                error: None,
            },
            Err(source) => Read {
                value: T::default(),
                error: Some(SettingsError::Folder {
                    path: self.folder.clone(),
                    source,
                }),
            },
        }
    }

    /// The canonical folder, created first (with its parents) when missing.
    fn folder_for_writing(&self) -> Result<PathBuf, SettingsError> {
        let folder_error = |source| SettingsError::Folder {
            path: self.folder.clone(),
            source,
        };
        let folder = match fs::canonicalize(&self.folder) {
            Ok(folder) => folder,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.folder).map_err(folder_error)?;
                fs::canonicalize(&self.folder).map_err(folder_error)?
            }
            Err(source) => return Err(folder_error(source)),
        };
        if !folder.is_dir() {
            return Err(folder_error(io::Error::from(io::ErrorKind::NotADirectory)));
        }
        Ok(folder)
    }
}
