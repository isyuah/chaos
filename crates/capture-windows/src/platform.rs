//! Windows platform wiring: DPI awareness + handles to the capture/snap backends.

use crate::capture::WindowsCaptureBackend;
use crate::snap::WindowsSnapBackend;
use capture_platform_api::{CaptureBackend, SnapBackend};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// A ready-to-use Windows platform exposing the two shared backends.
pub struct WindowsPlatform {
    capture: WindowsCaptureBackend,
    snap: WindowsSnapBackend,
}

impl WindowsPlatform {
    pub fn new() -> Self {
        // Make GetDC/EnumDisplayMonitors return physical pixels and physical
        // monitor rects for mixed-DPI support (see ADR-0001 / coordinate docs).
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Self {
            capture: WindowsCaptureBackend::new(),
            snap: WindowsSnapBackend::new(),
        }
    }

    pub fn capture_backend(&self) -> &dyn CaptureBackend {
        &self.capture
    }

    pub fn snap_backend(&self) -> &dyn SnapBackend {
        &self.snap
    }
}
