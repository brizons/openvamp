//! S3 (F7.5, F8.3): the settings the desktop opens every session with,
//! from the configuration folder: each device's saved routing (or the
//! preset for its outputs) and the global bus settings, with any file that
//! cannot be used reported. Every test uses a temporary folder, never the
//! real configuration folder.

use app::testing::FakeOutput;
use app::{AudioOutput, DeviceInfo};
use domain::{Bus, BusLevel, BusSettings, Destination, Routing, RoutingFile, Volume};
use library::{BUS_SETTINGS_FILE, ConfigStore, ROUTING_FILE};
use openvamp_desktop::audio_settings::AudioSettings;

fn store(dir: &tempfile::TempDir) -> ConfigStore {
    ConfigStore::new(dir.path().join("config").join("OpenVamp"))
}

fn devices(fake: &FakeOutput) -> Vec<DeviceInfo> {
    fake.devices().expect("the fake lists its devices")
}

fn drums_at(db: f64) -> BusSettings {
    let mut buses = BusSettings::new();
    buses.set_level(
        Bus::Drums,
        BusLevel {
            volume: Volume::from_db(db),
            mute: false,
        },
    );
    buses
}

fn vocals_on(output: u16, outputs: u16) -> Routing {
    let mut routing = Routing::preset(outputs);
    routing.buses.insert(Bus::Vocals, Destination::Mono(output));
    routing
}

#[test]
fn a_device_with_no_saved_routing_plays_the_preset_for_its_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let settings = AudioSettings::load(Some(store(&dir)));
    let fake = FakeOutput::without_devices().with_device("Interface", 8, None);

    let options = settings.options(&devices(&fake));

    assert_eq!(options.device.as_deref(), Some("fake:1"));
    assert_eq!(options.routing, Some(Routing::preset(8)));
    assert_eq!(options.bus_settings, BusSettings::new());
    assert_eq!(settings.notices(), [] as [String; 0]);
}

#[test]
fn the_saved_routing_of_the_device_and_the_bus_settings_open_the_next_session() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeOutput::without_devices()
        .with_device("Laptop", 2, None)
        .with_device("Interface", 8, None);
    let mut settings = AudioSettings::load(Some(store(&dir)));
    settings.save_routing("fake:2", &vocals_on(3, 8)).unwrap();
    settings.save_bus_settings(&drums_at(-6.0)).unwrap();

    // The next session reads them back.
    let mut settings = AudioSettings::load(Some(store(&dir)));
    settings.choose_device(Some("fake:2"));
    let options = settings.options(&devices(&fake));

    assert_eq!(options.device.as_deref(), Some("fake:2"));
    assert_eq!(options.routing, Some(vocals_on(3, 8)));
    assert_eq!(options.bus_settings, drums_at(-6.0));
    // The other device keeps its preset: routing is saved per device.
    settings.choose_device(Some("fake:1"));
    assert_eq!(
        settings.options(&devices(&fake)).routing,
        Some(Routing::preset(2))
    );
}

#[test]
fn with_no_device_chosen_the_default_one_opens() {
    let dir = tempfile::tempdir().unwrap();
    let settings = AudioSettings::load(Some(store(&dir)));
    let mut listed = devices(&FakeOutput::new().with_device("Interface", 8, None));
    listed[0].is_default = false;
    listed[1].is_default = true;

    let options = settings.options(&listed);

    assert_eq!(options.device.as_deref(), Some("fake:2"));
    assert_eq!(options.routing, Some(Routing::preset(8)));
}

#[test]
fn with_no_device_listed_the_session_opens_plain_stereo_on_the_default_device() {
    let dir = tempfile::tempdir().unwrap();
    let settings = AudioSettings::load(Some(store(&dir)));

    let options = settings.options(&[]);

    assert_eq!(options.device, None);
    assert_eq!(options.routing, None);
}

#[test]
fn unusable_settings_files_are_reported_and_the_defaults_used() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    std::fs::create_dir_all(store.folder()).unwrap();
    std::fs::write(store.folder().join(BUS_SETTINGS_FILE), "not json").unwrap();
    std::fs::write(store.folder().join(ROUTING_FILE), "{").unwrap();

    let settings = AudioSettings::load(Some(store));

    let notices = settings.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        notices[0].starts_with("The bus levels could not be loaded, so every bus plays at 0 dB: "),
        "{notices:?}"
    );
    assert!(
        notices[1].starts_with(
            "The saved routings could not be loaded, so every device plays its preset: "
        ),
        "{notices:?}"
    );
    let options = settings.options(&devices(&FakeOutput::new()));
    assert_eq!(options.bus_settings, BusSettings::new());
    assert_eq!(options.routing, Some(Routing::preset(2)));
}

#[test]
fn without_a_settings_folder_the_settings_last_until_the_app_closes_and_it_says_so() {
    let mut settings = AudioSettings::load(None);

    assert_eq!(
        settings.notices(),
        [
            "No settings folder was found, so routing and bus levels are not saved after the app closes"
        ]
    );
    settings.save_bus_settings(&drums_at(-3.0)).unwrap();
    settings.save_routing("fake:1", &vocals_on(1, 2)).unwrap();
    let options = settings.options(&devices(&FakeOutput::new()));
    assert_eq!(options.bus_settings, drums_at(-3.0));
    assert_eq!(options.routing, Some(vocals_on(1, 2)));
}

#[test]
fn a_saved_routing_is_fitted_to_a_device_that_now_has_fewer_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = AudioSettings::load(Some(store(&dir)));
    settings.save_routing("fake:1", &vocals_on(7, 8)).unwrap();
    let fake = FakeOutput::without_devices().with_device("Interface", 4, None);

    let options = settings.options(&devices(&fake));

    let mut file = RoutingFile::new();
    file.set_routing("fake:1", vocals_on(7, 8));
    assert_eq!(options.routing, Some(file.routing("fake:1", 4)));
}

#[test]
fn the_buffer_size_chosen_opens_the_next_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = AudioSettings::load(Some(store(&dir)));
    assert_eq!(settings.buffer_frames(), 256);

    settings.set_buffer_frames(128);

    let options = settings.options(&devices(&FakeOutput::new()));
    assert_eq!(options.buffer_frames, 128);
}

fn interface() -> DeviceInfo {
    DeviceInfo {
        id: "usb:1".to_owned(),
        name: "Interface".to_owned(),
        output_channels: 8,
        is_default: false,
    }
}

fn laptop() -> DeviceInfo {
    DeviceInfo {
        id: "builtin:1".to_owned(),
        name: "Laptop".to_owned(),
        output_channels: 2,
        is_default: true,
    }
}

#[test]
fn a_device_list_that_fails_opens_the_default_device_in_plain_stereo_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = AudioSettings::load(Some(store(&dir)));
    settings.choose(&interface());
    let error = app::OutputError::NoDevice;

    let (options, notice) = settings.open_options(&Err(error.clone()));

    assert_eq!((options.device, options.routing), (None, None));
    assert_eq!(
        notice.as_deref(),
        Some(
            format!(
                "The output devices could not be listed ({error}), so \"Interface\" is not used: the system's default device plays in plain stereo"
            )
            .as_str()
        )
    );
    settings.choose_device(None);
    let (_, notice) = settings.open_options(&Err(error.clone()));
    assert_eq!(
        notice,
        Some(format!(
            "The output devices could not be listed ({error}), so the system's default device plays in plain stereo, without its saved routing"
        ))
    );
}

#[test]
fn a_chosen_device_that_is_not_connected_says_which_one_plays_instead() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = AudioSettings::load(Some(store(&dir)));
    settings.choose(&interface());

    let (options, notice) = settings.open_options(&Ok(vec![laptop()]));
    assert_eq!(options.device.as_deref(), Some("builtin:1"));
    assert_eq!(
        notice.as_deref(),
        Some("\"Interface\" is not connected, so \"Laptop\" plays instead")
    );

    let (options, notice) = settings.open_options(&Ok(Vec::new()));
    assert_eq!((options.device, options.routing), (None, None));
    assert_eq!(
        notice.as_deref(),
        Some(
            "\"Interface\" is not connected, so the system's default device plays in plain stereo"
        )
    );

    let (options, notice) = settings.open_options(&Ok(vec![laptop(), interface()]));
    assert_eq!(options.device.as_deref(), Some("usb:1"));
    assert_eq!(notice, None, "the device chosen plays");
}
