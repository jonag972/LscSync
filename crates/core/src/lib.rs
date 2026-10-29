//! LscSync pure logic: colors, Tuya protocol, profile and settings.
//!
//! This crate has no platform dependency and does no network or graphics I/O:
//! it can be tested anywhere, including Linux CI.

pub mod color;
pub mod profile;
pub mod protocol;
pub mod settings;
pub mod snapshot;

pub use color::{Hsv, Image, Rgb};
pub use profile::{Device, Profile, ProfileError};
pub use settings::{AudioHue, Mode, Settings};
pub use snapshot::{Snapshot, lsc_colors};
