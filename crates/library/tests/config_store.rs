//! S1, S2: the global bus settings and the per-device routing in the app's
//! configuration folder (F7.5, F8.3, L2). Every test uses a temporary folder,
//! never the real configuration folder.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use domain::{
    Bus, BusLevel, BusSettings, BusSettingsError, Destination, MixerFileError, RouteTarget,
    Routing, RoutingError, RoutingFile, RoutingValidationError, Volume,
};
use library::{
    BUS_SETTINGS_FILE, ConfigStore, Platform, ROUTING_FILE, SettingsError, config_folder,
};
use tempfile::TempDir;

fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let vars: HashMap<String, OsString> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| vars.get(name).cloned()
}

/// A store whose folder does not exist yet, two levels below a temporary
/// folder.
fn new_store() -> (TempDir, ConfigStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config").join("OpenVamp"));
    (dir, store)
}

fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn some_settings() -> BusSettings {
    let mut settings = BusSettings::new();
    settings.set_level(
        Bus::Drums,
        BusLevel {
            volume: Volume::from_db(-3.0),
            mute: false,
        },
    );
    settings.set_level(
        Bus::Vocals,
        BusLevel {
            volume: Volume::UNITY,
            mute: true,
        },
    );
    settings.master_volume = Volume::from_db(-1.5);
    settings
}

fn some_routing() -> RoutingFile {
    let mut routing = RoutingFile::new();
    routing.set_routing("USB interface", Routing::preset(8));
    routing.set_routing("Built-in Output", Routing::preset(2));
    routing
}

#[test]
fn macos_keeps_the_settings_in_application_support() {
    assert_eq!(
        config_folder(Platform::MacOs, env(&[("HOME", "/Users/ana")])),
        Some(PathBuf::from(
            "/Users/ana/Library/Application Support/OpenVamp"
        ))
    );
    assert_eq!(config_folder(Platform::MacOs, env(&[])), None);
    assert_eq!(config_folder(Platform::MacOs, env(&[("HOME", "")])), None);
}

#[test]
fn windows_keeps_the_settings_in_appdata() {
    let appdata = r"C:\Users\ana\AppData\Roaming";
    assert_eq!(
        config_folder(Platform::Windows, env(&[("APPDATA", appdata)])),
        Some(Path::new(appdata).join("OpenVamp"))
    );
    assert_eq!(
        config_folder(Platform::Windows, env(&[("HOME", r"C:\Users\ana")])),
        None,
        "only APPDATA counts"
    );
    assert_eq!(
        config_folder(Platform::Windows, env(&[("APPDATA", "")])),
        None
    );
}

#[test]
fn a_relative_home_or_appdata_gives_no_folder() {
    for home in ["ana", "./ana", "~/ana"] {
        assert_eq!(
            config_folder(Platform::MacOs, env(&[("HOME", home)])),
            None,
            "{home:?}"
        );
        assert_eq!(
            config_folder(Platform::Linux, env(&[("HOME", home)])),
            None,
            "{home:?}"
        );
        assert_eq!(
            config_folder(
                Platform::Linux,
                env(&[("XDG_CONFIG_HOME", "cfg"), ("HOME", home)])
            ),
            None,
            "{home:?}"
        );
    }
    for appdata in [r"AppData\Roaming", "C:AppData", r"\Users\ana", "/Users/ana"] {
        assert_eq!(
            config_folder(Platform::Windows, env(&[("APPDATA", appdata)])),
            None,
            "{appdata:?}"
        );
    }
    for appdata in [
        r"C:\Users\ana\AppData\Roaming",
        "c:/Users/ana/AppData/Roaming",
        r"\\server\share\ana\AppData",
    ] {
        assert_eq!(
            config_folder(Platform::Windows, env(&[("APPDATA", appdata)])),
            Some(Path::new(appdata).join("OpenVamp")),
            "{appdata:?}"
        );
    }
}

#[test]
fn linux_follows_xdg_config_home_then_dot_config() {
    assert_eq!(
        config_folder(
            Platform::Linux,
            env(&[("XDG_CONFIG_HOME", "/home/ana/.cfg"), ("HOME", "/home/ana")])
        ),
        Some(PathBuf::from("/home/ana/.cfg/openvamp"))
    );
    for xdg in ["", "relative/cfg"] {
        assert_eq!(
            config_folder(
                Platform::Linux,
                env(&[("XDG_CONFIG_HOME", xdg), ("HOME", "/home/ana")])
            ),
            Some(PathBuf::from("/home/ana/.config/openvamp")),
            "an empty or relative XDG_CONFIG_HOME is ignored: {xdg:?}"
        );
    }
    assert_eq!(
        config_folder(Platform::Linux, env(&[("HOME", "/home/ana")])),
        Some(PathBuf::from("/home/ana/.config/openvamp"))
    );
    assert_eq!(config_folder(Platform::Linux, env(&[])), None);
}

#[test]
fn the_store_from_the_environment_uses_this_platform_s_folder() {
    let expected = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    };
    assert_eq!(Platform::current(), expected);
    // Reads the environment only; nothing is created or written.
    assert_eq!(
        ConfigStore::from_environment().map(|store| store.folder().to_owned()),
        config_folder(Platform::current(), |name| std::env::var_os(name))
    );
}

#[test]
fn missing_files_and_a_missing_folder_read_as_the_defaults() {
    let (_dir, store) = new_store();
    let buses = store.load_bus_settings();
    assert_eq!(buses.settings, BusSettings::new());
    assert!(buses.error.is_none() && buses.problems.is_empty());
    let routing = store.load_routing();
    assert_eq!(routing.routing, RoutingFile::new());
    assert!(routing.error.is_none() && routing.problems.is_empty());
    assert!(!store.folder().exists(), "reading creates nothing");

    fs::create_dir_all(store.folder()).unwrap();
    assert_eq!(store.load_bus_settings().settings, BusSettings::new());
    assert!(store.load_routing().error.is_none());
}

#[test]
fn settings_round_trip_and_the_folder_is_created_on_the_first_write() {
    let (_dir, store) = new_store();
    store.save_bus_settings(&some_settings()).expect("saved");
    assert_eq!(
        entries(store.folder()),
        [BUS_SETTINGS_FILE],
        "no temporary file"
    );
    let loaded = store.load_bus_settings();
    assert_eq!(loaded.settings, some_settings());
    assert!(loaded.error.is_none() && loaded.problems.is_empty());

    store.save_routing(&some_routing()).expect("saved");
    assert_eq!(entries(store.folder()), [BUS_SETTINGS_FILE, ROUTING_FILE]);
    let loaded = store.load_routing();
    assert_eq!(loaded.routing, some_routing());
    assert!(loaded.error.is_none() && loaded.problems.is_empty());
    assert_eq!(
        loaded.routing.routing("USB interface", 8),
        Routing::preset(8),
        "saved per device"
    );

    // Each file is written alone.
    let bus_settings = fs::read(store.folder().join(BUS_SETTINGS_FILE)).unwrap();
    store.save_routing(&RoutingFile::new()).unwrap();
    assert_eq!(
        fs::read(store.folder().join(BUS_SETTINGS_FILE)).unwrap(),
        bus_settings
    );
    assert_eq!(store.load_routing().routing, RoutingFile::new());
}

#[test]
fn files_that_break_rules_are_used_and_their_problems_reported() {
    let (_dir, store) = new_store();
    let mut settings = some_settings();
    settings.set_level(
        Bus::Bass,
        BusLevel {
            volume: Volume::from_db(20.0),
            mute: false,
        },
    );
    store.save_bus_settings(&settings).unwrap();
    let loaded = store.load_bus_settings();
    assert_eq!(
        loaded.settings, settings,
        "kept: gains are capped when playing"
    );
    assert!(loaded.error.is_none());
    assert_eq!(
        loaded.problems,
        [BusSettingsError::BusVolumeOutOfRange {
            bus: Bus::Bass,
            db: 20.0
        }]
    );

    let mut routing = some_routing();
    let mut usb = Routing::preset(8);
    usb.buses.insert(Bus::Drums, Destination::Mono(40));
    routing.set_routing("USB interface", usb);
    store.save_routing(&routing).unwrap();
    let loaded = store.load_routing();
    assert_eq!(loaded.routing, routing, "kept: a device plays it fitted");
    assert_eq!(
        loaded.problems,
        [RoutingValidationError::Device {
            device: "USB interface".to_owned(),
            error: RoutingError::OutputOutOfRange {
                target: RouteTarget::Bus(Bus::Drums),
                output: 40,
                outputs: 8
            }
        }]
    );
}

#[test]
fn a_malformed_file_falls_back_to_the_defaults_and_is_left_until_a_change() {
    let (_dir, store) = new_store();
    fs::create_dir_all(store.folder()).unwrap();
    let bus_path = store.folder().join(BUS_SETTINGS_FILE);
    let routing_path = store.folder().join(ROUTING_FILE);
    fs::write(&bus_path, "{ broken").unwrap();
    fs::write(&routing_path, r#"{ "formatVersion": 1, "devices": 3 }"#).unwrap();

    let buses = store.load_bus_settings();
    assert_eq!(buses.settings, BusSettings::new());
    assert!(
        matches!(&buses.error, Some(SettingsError::Format { path, source: MixerFileError::Json { .. } }) if path.ends_with(BUS_SETTINGS_FILE)),
        "{:?}",
        buses.error
    );
    let routing = store.load_routing();
    assert_eq!(routing.routing, RoutingFile::new());
    assert!(
        matches!(routing.error, Some(SettingsError::Format { .. })),
        "{:?}",
        routing.error
    );
    assert_eq!(fs::read_to_string(&bus_path).unwrap(), "{ broken");
    assert_eq!(
        fs::read_to_string(&routing_path).unwrap(),
        r#"{ "formatVersion": 1, "devices": 3 }"#
    );

    // When the user changes a setting, the new file replaces the broken one.
    store.save_bus_settings(&some_settings()).unwrap();
    assert_eq!(store.load_bus_settings().settings, some_settings());
    store.save_routing(&some_routing()).unwrap();
    assert_eq!(store.load_routing().routing, some_routing());
}

#[test]
fn a_newer_file_is_reported_and_never_overwritten() {
    let (_dir, store) = new_store();
    fs::create_dir_all(store.folder()).unwrap();
    let newer = r#"{ "formatVersion": 9, "anything": [] }"#;
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        fs::write(store.folder().join(file), newer).unwrap();
    }

    let buses = store.load_bus_settings();
    assert_eq!(buses.settings, BusSettings::new());
    assert!(
        matches!(
            buses.error,
            Some(SettingsError::Format {
                source: MixerFileError::UnsupportedFormatVersion { found: 9, .. },
                ..
            })
        ),
        "{:?}",
        buses.error
    );
    let error = store
        .save_bus_settings(&some_settings())
        .expect_err("refused");
    assert!(
        matches!(
            error,
            SettingsError::NewerVersion {
                found: 9,
                supported: 1,
                ..
            }
        ),
        "{error:?}"
    );

    assert_eq!(store.load_routing().routing, RoutingFile::new());
    let error = store.save_routing(&some_routing()).expect_err("refused");
    assert!(
        matches!(error, SettingsError::NewerVersion { found: 9, .. }),
        "{error:?}"
    );

    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        assert_eq!(
            fs::read_to_string(store.folder().join(file)).unwrap(),
            newer
        );
    }
    assert_eq!(entries(store.folder()), [BUS_SETTINGS_FILE, ROUTING_FILE]);
}

#[test]
fn a_settings_file_too_large_to_check_is_not_written_over() {
    let (_dir, store) = new_store();
    fs::create_dir_all(store.folder()).unwrap();
    let large = format!(
        r#"{{ "formatVersion": 9, "pad": "{}" }}"#,
        "x".repeat(library::MAX_SETTINGS_FILE_BYTES as usize)
    );
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        fs::write(store.folder().join(file), &large).unwrap();
    }
    let errors = [
        store
            .save_bus_settings(&some_settings())
            .expect_err("unchecked"),
        store.save_routing(&some_routing()).expect_err("unchecked"),
    ];
    for error in &errors {
        assert!(
            matches!(error, SettingsError::UncheckedVersion { source, .. } if matches!(**source, SettingsError::TooLarge { .. })),
            "{error:?}"
        );
    }
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        assert_eq!(
            fs::read_to_string(store.folder().join(file)).unwrap(),
            large
        );
    }
    assert_eq!(entries(store.folder()), [BUS_SETTINGS_FILE, ROUTING_FILE]);
}

#[cfg(unix)]
#[test]
fn a_settings_file_the_check_cannot_read_is_not_written_over() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, store) = new_store();
    fs::create_dir_all(store.folder()).unwrap();
    let newer = r#"{"formatVersion":2}"#;
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        let path = store.folder().join(file);
        fs::write(&path, newer).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    }
    if fs::read(store.folder().join(BUS_SETTINGS_FILE)).is_ok() {
        for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
            fs::set_permissions(store.folder().join(file), fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        eprintln!(
            "note: mode 0o000 does not block reads here (root or CAP_DAC_OVERRIDE); skipping"
        );
        return;
    }
    let results = [
        store.save_bus_settings(&some_settings()),
        store.save_routing(&some_routing()),
    ];
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        fs::set_permissions(store.folder().join(file), fs::Permissions::from_mode(0o644)).unwrap();
    }
    for result in &results {
        assert!(
            matches!(result, Err(SettingsError::UncheckedVersion { .. })),
            "{result:?}"
        );
    }
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        assert_eq!(
            fs::read_to_string(store.folder().join(file)).unwrap(),
            newer
        );
    }
}

#[test]
fn a_settings_file_that_is_not_utf8_is_reported_and_replaced_on_save() {
    let (_dir, store) = new_store();
    fs::create_dir_all(store.folder()).unwrap();
    let mut utf16 = vec![0xFF, 0xFE];
    utf16.extend(r#"{"formatVersion":2}"#.encode_utf16().flat_map(u16::to_le_bytes));
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        fs::write(store.folder().join(file), &utf16).unwrap();
    }

    let buses = store.load_bus_settings();
    assert_eq!(buses.settings, BusSettings::new());
    assert!(
        matches!(buses.error, Some(SettingsError::NotText { .. })),
        "{:?}",
        buses.error
    );
    let routing = store.load_routing();
    assert_eq!(routing.routing, RoutingFile::new());
    assert!(
        matches!(routing.error, Some(SettingsError::NotText { .. })),
        "{:?}",
        routing.error
    );
    for file in [BUS_SETTINGS_FILE, ROUTING_FILE] {
        assert_eq!(
            fs::read(store.folder().join(file)).unwrap(),
            utf16,
            "left on load"
        );
    }

    store.save_bus_settings(&some_settings()).expect("replaced");
    store.save_routing(&some_routing()).expect("replaced");
    assert_eq!(store.load_bus_settings().settings, some_settings());
    assert_eq!(store.load_routing().routing, some_routing());
}

#[test]
fn a_refused_save_creates_no_folder() {
    let (dir, store) = new_store();
    let mut settings = some_settings();
    settings.master_volume = Volume::from_db(f64::NAN);
    let error = store
        .save_bus_settings(&settings)
        .expect_err("JSON cannot hold NaN");
    assert!(matches!(error, SettingsError::Format { .. }), "{error:?}");
    assert!(!store.folder().exists());
    assert!(!dir.path().join("config").exists(), "nor its parents");
}

#[test]
fn a_failed_write_leaves_no_temporary_file() {
    let (_dir, store) = new_store();
    fs::create_dir_all(store.folder().join(ROUTING_FILE)).unwrap();
    let error = store
        .save_routing(&some_routing())
        .expect_err("a folder is in the way");
    assert!(matches!(error, SettingsError::Write { .. }), "{error:?}");
    assert_eq!(entries(store.folder()), [ROUTING_FILE]);
    assert!(entries(&store.folder().join(ROUTING_FILE)).is_empty());
}

#[test]
fn a_configuration_folder_that_is_a_file_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("OpenVamp");
    fs::write(&file, "not a folder").unwrap();
    let store = ConfigStore::new(&file);
    let error = store
        .save_bus_settings(&some_settings())
        .expect_err("not a folder");
    assert!(matches!(error, SettingsError::Folder { .. }), "{error:?}");
    assert!(store.load_bus_settings().error.is_some());
    assert_eq!(fs::read_to_string(&file).unwrap(), "not a folder");
}

#[cfg(unix)]
#[test]
fn a_settings_file_linked_from_outside_the_folder_is_refused_and_left() {
    use std::os::unix::fs::symlink;

    let (dir, store) = new_store();
    fs::create_dir_all(store.folder()).unwrap();
    let outside = dir.path().join("elsewhere.json");
    let contents = some_settings().to_json().unwrap();
    fs::write(&outside, &contents).unwrap();
    symlink(&outside, store.folder().join(BUS_SETTINGS_FILE)).unwrap();

    let loaded = store.load_bus_settings();
    assert_eq!(loaded.settings, BusSettings::new());
    assert!(
        matches!(loaded.error, Some(SettingsError::Outside { .. })),
        "{:?}",
        loaded.error
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), contents);
}
