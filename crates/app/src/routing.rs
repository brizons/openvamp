//! The routing a session plays with (F8.2-F8.4): the one the shell gave, or
//! plain stereo, always fitted to the device before it is used.

use domain::{Bus, MAX_OUTPUTS, Routing};
use engine::live::Routes;

use crate::output::{AudioOutput, DeviceChoice};

/// The routing of a session and the channels it renders.
///
/// Every player of a session renders [`Self::channels`] channels, chosen
/// when the session is prepared: the outputs of the routing the shell gave
/// ([`Routing::outputs`]), or two for plain stereo. The routing in use is
/// the shell's fitted to the device's outputs, at most that many
/// ([`Routing::fit`]), so no bus is ever routed past what the players
/// render or the device has; plain stereo is [`Routing::stereo`], which
/// plays exactly what the players played before routing, and folds to mono
/// on a one-output device.
#[derive(Debug, Clone)]
pub(crate) struct Router {
    /// The routing the shell gave; `None` for plain stereo.
    base: Option<Routing>,
    /// Channels every player renders.
    channels: u16,
    /// The device's outputs, as last seen.
    outputs: u16,
    /// `base` fitted to `outputs`, at most `channels`.
    fitted: Routing,
}

impl Router {
    /// The router for `base`, fitted to its own outputs until a device is
    /// known.
    pub(crate) fn new(base: Option<Routing>) -> Self {
        let channels = player_channels(base.as_ref());
        let fitted = fit(base.as_ref(), channels, channels);
        Self {
            base,
            channels,
            outputs: channels,
            fitted,
        }
    }

    /// The routing in use.
    pub(crate) fn routing(&self) -> &Routing {
        &self.fitted
    }

    /// The routing in use, as the players take it.
    pub(crate) fn routes(&self) -> Routes {
        Routes::from_routing(&self.fitted)
    }

    /// Channels the output must take: the highest output the routing in
    /// use plays on, and two at least on a device with two or more.
    pub(crate) fn needed(&self) -> u16 {
        let highest = Bus::ALL
            .iter()
            .map(|&bus| self.fitted.destination(bus))
            .chain(self.fitted.master_destination())
            .flat_map(|destination| destination.outputs())
            .max()
            .unwrap_or(1);
        highest.max(self.fitted.outputs.min(2))
    }

    /// Fits the routing to a device with `outputs` outputs; whether the
    /// routing in use changed.
    pub(crate) fn fit_to(&mut self, outputs: u16) -> bool {
        self.outputs = outputs;
        self.refit()
    }

    /// Plays `routing` from now on, fitted to the device; whether the
    /// routing in use changed.
    pub(crate) fn set(&mut self, routing: Routing) -> bool {
        self.base = Some(routing);
        self.refit()
    }

    fn refit(&mut self) -> bool {
        let fitted = fit(self.base.as_ref(), self.outputs, self.channels);
        let changed = fitted != self.fitted;
        self.fitted = fitted;
        changed
    }
}

/// Channels the players of a session with `routing` render: its outputs,
/// or two for plain stereo.
pub(crate) fn player_channels(routing: Option<&Routing>) -> u16 {
    routing.map_or(2, |routing| routing.outputs.clamp(1, MAX_OUTPUTS))
}

/// `base` fitted to `outputs`, at most `channels`.
fn fit(base: Option<&Routing>, outputs: u16, channels: u16) -> Routing {
    let outputs = outputs.min(channels).max(1);
    match base {
        Some(routing) => routing.fit(outputs),
        None => Routing::stereo(outputs),
    }
}

/// The outputs of the device `choice` means (the default output for
/// `None`), as `output` lists it; `None` when it cannot tell: the list
/// cannot be read or does not have the device.
pub(crate) fn device_outputs(
    output: &dyn AudioOutput,
    choice: Option<&DeviceChoice>,
) -> Option<u16> {
    let devices = output.devices().ok()?;
    let index = match choice {
        None => devices.iter().position(|device| device.is_default)?,
        Some(choice) => {
            let keys: Vec<(&str, &str)> = devices
                .iter()
                .map(|device| (device.id.as_str(), device.name.as_str()))
                .collect();
            choice.find(&keys).ok()?
        }
    };
    Some(devices[index].output_channels).filter(|&outputs| outputs > 0)
}
