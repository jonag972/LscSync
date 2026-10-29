//! Colors shown in the app while the sync runs.

use crate::color::Hsv;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Every connected device, with the color of each of its zones.
    pub devices: Vec<DeviceColors>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceColors {
    pub id: String,
    pub colors: Vec<Hsv>,
}

impl Snapshot {
    /// Colors of one device's zones, if it is connected.
    pub fn colors(&self, id: &str) -> Option<&[Hsv]> {
        self.devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.colors.as_slice())
    }

    /// Every zone color of every device.
    pub fn all(&self) -> impl Iterator<Item = &Hsv> {
        self.devices.iter().flat_map(|device| &device.colors)
    }
}
