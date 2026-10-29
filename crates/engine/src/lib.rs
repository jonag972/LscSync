//! Sync engine: Tuya connection, screen and audio capture, sync thread.
//! Independent of the UI.

pub mod audio;
pub mod capture;
pub mod tuya;
pub mod worker;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
