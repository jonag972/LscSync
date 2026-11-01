//! Watches the default sound output device and reports every change, so
//! Music mode follows it live: the automatic Bluetooth delay, and on
//! Windows the loopback capture that is tied to that device.
//!
//! macOS: a Core Audio listener on the default output device. Windows:
//! `IMMNotificationClient::OnDefaultDeviceChanged`. Linux: `pactl
//! subscribe`. When the native listener cannot be installed, a slow poll
//! compares the default device's id.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use super::output;

/// The default output device changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputChange {
    /// Whether the new default output is a Bluetooth one.
    pub bluetooth: bool,
}

/// How often the fallback poll reads the default output device.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A running watch: dropping it stops the listener.
pub struct OutputWatch {
    receiver: Receiver<OutputChange>,
    /// The platform handle, stopped when it is dropped with the watch.
    _handle: Box<dyn Send>,
}

impl OutputWatch {
    /// Starts watching. Always succeeds: the fallback poll runs when the
    /// native listener cannot be installed.
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        // The current state first, so the engine starts in sync with the
        // device the user is listening through right now.
        let _ = sender.send(current());
        let handle: Box<dyn Send> = match platform::start(sender.clone()) {
            Some(handle) => Box::new(handle),
            None => {
                tracing::info!(
                    "watching the sound output by polling every {} s",
                    POLL_INTERVAL.as_secs()
                );
                Box::new(poll::start(sender))
            }
        };
        Self {
            receiver,
            _handle: handle,
        }
    }

    /// The changes since the last read.
    pub fn receiver(&self) -> &Receiver<OutputChange> {
        &self.receiver
    }
}

/// The default output right now.
fn current() -> OutputChange {
    OutputChange {
        bluetooth: output::output_is_bluetooth(),
    }
}

/// Reports the default output as it is now.
fn report(sender: &Sender<OutputChange>) {
    let _ = sender.send(current());
}

/// Fallback when no native listener is available: compare the default
/// device's id every `POLL_INTERVAL`.
mod poll {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    pub struct Handle {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    pub fn start(sender: Sender<OutputChange>) -> Handle {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut last = output::default_output_device_id();
            while !stop_thread.load(Ordering::Relaxed) {
                std::thread::sleep(POLL_INTERVAL);
                let now = output::default_output_device_id();
                if now != last {
                    last = now;
                    report(&sender);
                }
            }
        });
        Handle {
            stop,
            thread: Some(thread),
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! Core Audio: a listener on the default output device property.

    use super::*;
    use std::ffi::c_void;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    type Listener = unsafe extern "C" fn(u32, u32, *const PropertyAddress, *mut c_void) -> i32;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectAddPropertyListener(
            object: u32,
            address: *const PropertyAddress,
            listener: Listener,
            client_data: *mut c_void,
        ) -> i32;
        fn AudioObjectRemovePropertyListener(
            object: u32,
            address: *const PropertyAddress,
            listener: Listener,
            client_data: *mut c_void,
        ) -> i32;
    }

    const fn code(name: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*name)
    }

    const SYSTEM_OBJECT: u32 = 1;
    const SCOPE_GLOBAL: u32 = code(b"glob");
    const DEFAULT_OUTPUT_DEVICE: u32 = code(b"dOut");

    fn default_output_address() -> PropertyAddress {
        PropertyAddress {
            selector: DEFAULT_OUTPUT_DEVICE,
            scope: SCOPE_GLOBAL,
            element: 0,
        }
    }

    /// Runs on a Core Audio thread: no panics, just report the new device.
    unsafe extern "C" fn on_default_output_changed(
        _object: u32,
        _count: u32,
        _addresses: *const PropertyAddress,
        client_data: *mut c_void,
    ) -> i32 {
        // SAFETY: installed by `start`, valid until `drop` removes it.
        let sender = unsafe { &*(client_data as *const Sender<OutputChange>) };
        report(sender);
        0
    }

    pub struct Handle {
        client_data: *mut c_void,
    }

    // The listener runs on Core Audio threads; the pointer is only handed
    // back to Core Audio, never dereferenced here.
    unsafe impl Send for Handle {}

    pub fn start(sender: Sender<OutputChange>) -> Option<Handle> {
        let client_data = Box::into_raw(Box::new(sender)) as *mut c_void;
        let address = default_output_address();
        // SAFETY: the listener only reads the sender through client_data.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                SYSTEM_OBJECT,
                &address,
                on_default_output_changed,
                client_data,
            )
        };
        if status == 0 {
            Some(Handle { client_data })
        } else {
            // SAFETY: the listener was not installed, the box is ours again.
            unsafe { drop(Box::from_raw(client_data as *mut Sender<OutputChange>)) };
            tracing::warn!("Core Audio listener not installed (status {status}), polling instead");
            None
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            let address = default_output_address();
            unsafe {
                AudioObjectRemovePropertyListener(
                    SYSTEM_OBJECT,
                    &address,
                    on_default_output_changed,
                    self.client_data,
                );
                // Core Audio may still be running the listener on another
                // thread while removing it: the sender is tiny and created
                // once per engine run, so leave it rather than free it under
                // a running callback.
                std::mem::forget(Box::from_raw(self.client_data as *mut Sender<OutputChange>));
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    //! WASAPI: `OnDefaultDeviceChanged` from an `IMMNotificationClient`.

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    use windows::core::{PCWSTR, implement};

    #[implement(IMMNotificationClient)]
    struct Client {
        sender: Sender<OutputChange>,
    }

    #[allow(non_snake_case)]
    impl IMMNotificationClient_Impl for Client_Impl {
        fn OnDeviceStateChanged(
            &self,
            _id: &PCWSTR,
            _new_state: DEVICE_STATE,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            _role: ERole,
            _id: &PCWSTR,
        ) -> windows::core::Result<()> {
            if flow == eRender {
                // The callbacks come on a COM thread: initialize it.
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                report(&self.sender);
            }
            Ok(())
        }

        fn OnPropertyValueChanged(
            &self,
            _id: &PCWSTR,
            _key: &PROPERTYKEY,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }

    pub struct Handle {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    pub fn start(sender: Sender<OutputChange>) -> Option<Handle> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let (ready, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            // SAFETY: COM calls on this dedicated thread; the client stays
            // alive in this scope until the watch stops.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let enumerator: IMMDeviceEnumerator =
                    match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                        Ok(enumerator) => enumerator,
                        Err(_) => {
                            let _ = ready.send(Err(()));
                            return;
                        }
                    };
                let client: IMMNotificationClient = Client { sender }.into();
                if enumerator
                    .RegisterEndpointNotificationCallback(&client)
                    .is_err()
                {
                    let _ = ready.send(Err(()));
                    return;
                }
                let _ = ready.send(Ok(()));
                while !stop_thread.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
                CoUninitialize();
            }
        });
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Some(Handle {
                stop,
                thread: Some(thread),
            }),
            _ => {
                tracing::warn!("WASAPI notification client not installed, polling instead");
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                None
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! PulseAudio / PipeWire: `pactl subscribe` reports the server changes,
    //! which is where the default sink changes.

    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    pub struct Handle {
        stop: Arc<AtomicBool>,
        child: Child,
        thread: Option<JoinHandle<()>>,
    }

    pub fn start(sender: Sender<OutputChange>) -> Option<Handle> {
        let mut child = Command::new("pactl")
            .arg("subscribe")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut last = output::default_output_device_id();
            for line in BufReader::new(stdout).lines() {
                if stop_thread.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(line) = line else { break };
                // The default sink changes come as server events.
                if line.contains("on server") {
                    let now = output::default_output_device_id();
                    if now != last {
                        last = now;
                        report(&sender);
                    }
                }
            }
        });
        Some(Handle {
            stop,
            child,
            thread: Some(thread),
        })
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;

    pub struct Handle;

    pub fn start(_sender: Sender<OutputChange>) -> Option<Handle> {
        None
    }
}
