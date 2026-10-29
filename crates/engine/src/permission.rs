//! TCC consent on macOS: the capture APIs (CGWindowListCreateImage,
//! ScreenCaptureKit audio) fail silently without the "Screen & System Audio
//! Recording" grant, and the system dialog only appears when the app asks for
//! it explicitly.

/// Returns true when screen recording is granted. When it is not, asks for it:
/// the first call makes macOS show its consent dialog and create the TCC entry.
/// Call this when a mode starts, before capturing anything.
pub fn ensure_screen_capture_access() -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

        if CGPreflightScreenCaptureAccess() {
            return true;
        }
        // Shows the system consent dialog if this app has not been asked yet.
        // The return value is the status before the user answers, so re-check.
        CGRequestScreenCaptureAccess();
        let granted = CGPreflightScreenCaptureAccess();
        if granted {
            tracing::info!("screen capture access granted");
        } else {
            tracing::warn!(
                "screen capture access not granted yet: allow LscSync in System Settings, then start again"
            );
        }
        granted
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}
