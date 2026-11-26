//! The settings every session opens with (S3, F7.5, F8.3): the routing
//! saved for each output device, keyed by the device's id, and the global
//! bus settings, from the app's configuration folder
//! ([`library::ConfigStore`]); the device and buffer size chosen on the
//! Audio output screen. A file that cannot be used is reported and its
//! defaults are played; with no configuration folder the settings last
//! until the app closes.

use app::{DeviceInfo, OutputError, SessionOptions};
use domain::{BusSettings, Routing, RoutingFile};
use library::ConfigStore;

/// The routing and bus settings the desktop plays with, and where they are
/// saved.
#[derive(Debug)]
pub struct AudioSettings {
    store: Option<ConfigStore>,
    routing: RoutingFile,
    buses: BusSettings,
    notices: Vec<String>,
    device: Option<String>,
    /// The chosen device's name, for messages.
    device_name: Option<String>,
    buffer_frames: u32,
}

impl AudioSettings {
    /// Reads the settings from `store`, the configuration folder
    /// ([`ConfigStore::from_environment`]); `None` when there is none, so
    /// the defaults play and nothing is saved after the app closes.
    pub fn load(store: Option<ConfigStore>) -> Self {
        let mut notices = Vec::new();
        let (routing, buses) = match &store {
            Some(store) => {
                let buses = store.load_bus_settings();
                if let Some(error) = &buses.error {
                    notices.push(format!(
                        "The bus levels could not be loaded, so every bus plays at 0 dB: {error}"
                    ));
                }
                notices.extend(
                    buses
                        .problems
                        .iter()
                        .map(|problem| format!("Bus levels: {problem}")),
                );
                let routing = store.load_routing();
                if let Some(error) = &routing.error {
                    notices.push(format!(
                        "The saved routings could not be loaded, so every device plays its preset: {error}"
                    ));
                }
                notices.extend(
                    routing
                        .problems
                        .iter()
                        .map(|problem| format!("Saved routing: {problem}")),
                );
                (routing.routing, buses.settings)
            }
            None => {
                notices.push(
                    "No settings folder was found, so routing and bus levels are not saved after the app closes"
                        .to_owned(),
                );
                (RoutingFile::new(), BusSettings::new())
            }
        };
        Self {
            store,
            routing,
            buses,
            notices,
            device: None,
            device_name: None,
            buffer_frames: SessionOptions::default().buffer_frames,
        }
    }

    /// Why a settings file is not used, or not saved, one line each.
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// The options a session opens with on one of `devices`: the device
    /// chosen ([`Self::choose_device`]) when it is listed, else the default
    /// one, else the first; its saved routing fitted to its outputs, or the
    /// preset for them; the bus settings and the buffer size. With no device
    /// listed, the default device plays plain stereo.
    pub fn options(&self, devices: &[DeviceInfo]) -> SessionOptions {
        let device = self.device(devices);
        SessionOptions {
            device: device.map(|device| device.id.clone()),
            routing: device.map(|device| self.routing_for(device)),
            bus_settings: self.buses.clone(),
            buffer_frames: self.buffer_frames,
            ..SessionOptions::default()
        }
    }

    /// [`Self::options`] for the devices the system `listed`, and why the
    /// session does not open where it was asked to, when it does not: the
    /// list failed (the system's default device then plays in plain
    /// stereo), or the chosen device is not connected.
    pub fn open_options(
        &self,
        listed: &Result<Vec<DeviceInfo>, OutputError>,
    ) -> (SessionOptions, Option<String>) {
        let wanted = self
            .device
            .as_ref()
            .map(|id| format!("\"{}\"", self.device_name.as_deref().unwrap_or(id)));
        let devices = match listed {
            Ok(devices) => devices,
            Err(error) => {
                let notice = match wanted {
                    Some(wanted) => format!(
                        "The output devices could not be listed ({error}), so {wanted} is not used: the system's default device plays in plain stereo"
                    ),
                    None => format!(
                        "The output devices could not be listed ({error}), so the system's default device plays in plain stereo, without its saved routing"
                    ),
                };
                return (self.options(&[]), Some(notice));
            }
        };
        let options = self.options(devices);
        let notice = wanted
            .filter(|_| options.device != self.device)
            .map(|wanted| match self.device(devices) {
                Some(used) => format!("{wanted} is not connected, so \"{}\" plays instead", used.name),
                None => format!(
                    "{wanted} is not connected, so the system's default device plays in plain stereo"
                ),
            });
        (options, notice)
    }

    /// The device of `devices` a session opens on ([`Self::options`]).
    pub fn device<'a>(&self, devices: &'a [DeviceInfo]) -> Option<&'a DeviceInfo> {
        let chosen = self
            .device
            .as_deref()
            .and_then(|id| devices.iter().find(|device| device.id == id));
        chosen
            .or_else(|| devices.iter().find(|device| device.is_default))
            .or_else(|| devices.first())
    }

    /// The routing `device` plays with: its saved routing fitted to its
    /// outputs, or the preset for them.
    pub fn routing_for(&self, device: &DeviceInfo) -> Routing {
        self.routing.routing(&device.id, device.output_channels)
    }

    /// Chooses the device with the id `device` for the next sessions; `None`
    /// for the system's default.
    pub fn choose_device(&mut self, device: Option<&str>) {
        self.device = device.map(str::to_owned);
        self.device_name = None;
    }

    /// Chooses `device` for the next sessions, by its id; its name names it
    /// in messages.
    pub fn choose(&mut self, device: &DeviceInfo) {
        self.device = Some(device.id.clone());
        self.device_name = Some(device.name.clone());
    }

    /// The buffer size, in frames, the next sessions open with.
    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    /// Sets the buffer size the next sessions open with.
    pub fn set_buffer_frames(&mut self, frames: u32) {
        self.buffer_frames = frames;
    }

    /// Keeps `routing` as the routing of the device with the id `device`
    /// and saves every device's routing; why it could not be saved.
    pub fn save_routing(&mut self, device: &str, routing: &Routing) -> Result<(), String> {
        self.routing.set_routing(device, routing.clone());
        match &self.store {
            Some(store) => store
                .save_routing(&self.routing)
                .map_err(|error| error.to_string()),
            None => Ok(()),
        }
    }

    /// Keeps `buses` as the global bus settings and saves them; why they
    /// could not be saved.
    pub fn save_bus_settings(&mut self, buses: &BusSettings) -> Result<(), String> {
        self.buses = buses.clone();
        match &self.store {
            Some(store) => store
                .save_bus_settings(buses)
                .map_err(|error| error.to_string()),
            None => Ok(()),
        }
    }
}
