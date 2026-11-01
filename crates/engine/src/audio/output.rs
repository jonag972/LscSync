//! Is the sound played through Bluetooth? Bluetooth speakers and headphones
//! play the sound 150 to 300 ms after the computer sends it, while the
//! lights follow it at once: Music mode then delays the lights.

use std::time::Duration;

/// A typical Bluetooth delay, a starting point the user adjusts by ear:
/// neither macOS nor Windows report the speaker's real latency.
pub const BLUETOOTH_DELAY: Duration = Duration::from_millis(200);

/// The delay to apply when the user did not choose one.
pub fn automatic_delay() -> Duration {
    delay_for(output_is_bluetooth())
}

/// The automatic delay for an output of this kind: a Bluetooth speaker
/// plays the sound late, so the lights wait.
pub fn delay_for(bluetooth: bool) -> Duration {
    if bluetooth {
        BLUETOOTH_DELAY
    } else {
        Duration::ZERO
    }
}

/// Identifier of the default output device, when it can be told. Comparing
/// two reads notices a change of device.
pub fn default_output_device_id() -> Option<String> {
    platform::default_output_device_id()
}

/// Whether the default output device is a Bluetooth one. False when it
/// cannot be told.
pub fn output_is_bluetooth() -> bool {
    let bluetooth = platform::output_is_bluetooth();
    tracing::info!(
        "sound output: {}",
        if bluetooth {
            "Bluetooth"
        } else {
            "not Bluetooth"
        }
    );
    bluetooth
}

#[cfg(target_os = "macos")]
mod platform {
    //! Core Audio: the default output device's transport type.

    use std::ffi::c_void;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    const fn code(name: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*name)
    }

    const SYSTEM_OBJECT: u32 = 1;
    const SCOPE_GLOBAL: u32 = code(b"glob");
    const DEFAULT_OUTPUT_DEVICE: u32 = code(b"dOut");
    const TRANSPORT_TYPE: u32 = code(b"tran");
    const BLUETOOTH: u32 = code(b"blue");
    const BLUETOOTH_LE: u32 = code(b"blea");

    fn property(object: u32, selector: u32) -> Option<u32> {
        let address = PropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: 0,
        };
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: a u32 property read into a u32, with its size.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut value as *mut u32).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    pub fn output_is_bluetooth() -> bool {
        property(SYSTEM_OBJECT, DEFAULT_OUTPUT_DEVICE)
            .and_then(|device| property(device, TRANSPORT_TYPE))
            .is_some_and(|transport| transport == BLUETOOTH || transport == BLUETOOTH_LE)
    }

    pub fn default_output_device_id() -> Option<String> {
        property(SYSTEM_OBJECT, DEFAULT_OUTPUT_DEVICE).map(|device| device.to_string())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    //! WASAPI: the device the default output endpoint is connected to has a
    //! Bluetooth enumerator in its id (BTHENUM, BTHHFENUM, BTHLEDEVICE).

    use windows::Win32::Media::Audio::{
        IConnector, IDeviceTopology, IMMDeviceEnumerator, IPart, MMDeviceEnumerator, eConsole,
        eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    };
    use windows::core::Interface as _;

    fn connected_device_id() -> windows::core::Result<String> {
        // SAFETY: plain COM calls; the returned string is freed after copying.
        unsafe {
            // Already initialized on this thread (in another mode) is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let endpoint = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let topology: IDeviceTopology = endpoint.Activate(CLSCTX_ALL, None)?;
            let connector: IConnector = topology.GetConnector(0)?;
            let other: IConnector = connector.GetConnectedTo()?;
            let part: IPart = other.cast()?;
            let device: IDeviceTopology = part.GetTopologyObject()?;
            let id = device.GetDeviceId()?;
            let text = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as *const _));
            Ok(text)
        }
    }

    pub fn output_is_bluetooth() -> bool {
        match connected_device_id() {
            Ok(id) => id.to_ascii_lowercase().contains("bth"),
            Err(error) => {
                tracing::debug!("sound output: {error}");
                false
            }
        }
    }

    pub fn default_output_device_id() -> Option<String> {
        // SAFETY: plain COM calls; the returned string is freed after copying.
        unsafe {
            // Already initialized on this thread (in another mode) is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let endpoint = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            let id = endpoint.GetId().ok()?;
            let text = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as *const _));
            Some(text)
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! PulseAudio / PipeWire: Bluetooth sinks are named `bluez_…`.

    pub fn output_is_bluetooth() -> bool {
        default_output_device_id().is_some_and(|sink| sink.contains("bluez"))
    }

    pub fn default_output_device_id() -> Option<String> {
        std::process::Command::new("pactl")
            .arg("get-default-sink")
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|sink| !sink.is_empty())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub fn output_is_bluetooth() -> bool {
        false
    }

    pub fn default_output_device_id() -> Option<String> {
        None
    }
}
