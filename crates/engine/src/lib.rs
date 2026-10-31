//! Sync engine: Tuya connection, screen and audio capture, sync thread.
//! Independent of the UI.

pub mod audio;
pub mod capture;
mod latest;
pub mod permission;
pub mod tuya;
pub mod worker;

pub use latest::{Closed, Latest, LatestSender};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
