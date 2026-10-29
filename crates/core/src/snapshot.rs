//! Colors shown in the app while the sync runs.

use crate::color::Hsv;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Every zone of every synced device, in order (one color in Color mode,
    /// 3 in Music mode).
    pub zones: Vec<Hsv>,
}
