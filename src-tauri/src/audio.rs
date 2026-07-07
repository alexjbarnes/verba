use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub index: usize,
}

pub fn list_input_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    if let Ok(inputs) = host.input_devices() {
        for (i, dev) in inputs.enumerate() {
            // cpal 0.18 dropped DeviceTrait::name(); the human name now comes
            // from description() (or Display). Skip devices whose description
            // can't be read.
            if let Ok(name) = dev.description().map(|d| d.name().to_string()) {
                // Skip duplicate device names (common on Android where cpal
                // reports multiple input sources for the same physical mic).
                if seen_names.insert(name.clone()) {
                    devices.push(AudioDevice { name, index: i });
                }
            }
        }
    }

    devices
}
