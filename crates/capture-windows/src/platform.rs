//! Windows platform wiring: DPI awareness + handles to the capture/snap backends.

use crate::capture::WindowsCaptureBackend;
use crate::snap::WindowsSnapBackend;
use capture_core::CaptureError;
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
    pub fn new() -> Result<Self, CaptureError> {
        // Make GetDC/EnumDisplayMonitors return physical pixels and physical
        // monitor rects for mixed-DPI support (see ADR-0001 / coordinate docs).
        // DPI awareness is process-global. A frontend or application manifest
        // may have initialized it before constructing this backend; Windows
        // reports that as a failed second set, which is not a backend failure.
        // The call is therefore best-effort and the host owns the process
        // policy. Frontends should still declare Per-Monitor-V2 in their
        // manifest/startup path for physical-pixel coordinates.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Ok(Self {
            capture: WindowsCaptureBackend::new(),
            snap: WindowsSnapBackend::new(),
        })
    }

    pub fn capture_backend(&self) -> &dyn CaptureBackend {
        &self.capture
    }

    pub fn snap_backend(&self) -> &dyn SnapBackend {
        &self.snap
    }
}
