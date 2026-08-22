//! Linux backend implementation (compiled only on Linux).

use capture_core::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PhysicalPoint,
    SnapCandidate, SnapCapabilities, SnapError, SnapExclusionToken,
};
use capture_platform_api::{CaptureBackend, SnapBackend};

/// Linux capture backend. Reports `Unsupported` for the demo.
pub struct LinuxCaptureBackend;

impl CaptureBackend for LinuxCaptureBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            multi_monitor: false,
            per_monitor_dpi: false,
            capture_window: false,
            live_preview: false,
        }
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Err(CaptureError::Unsupported(
            "Linux capture is a demo skeleton: X11 XRandR / Wayland ScreenCast portal capture is not wired yet"
                .to_string(),
        ))
    }

    fn capture_monitor(&self, _id: MonitorId) -> Result<CapturedFrame, CaptureError> {
        Err(CaptureError::Unsupported(
            "Linux capture is a demo skeleton: implement X11/XWayland XShm or the XDG portal route behind this trait"
                .to_string(),
        ))
    }
}

/// Linux snap backend. Reports `Unsupported` for the demo.
pub struct LinuxSnapBackend;

impl SnapBackend for LinuxSnapBackend {
    fn capabilities(&self) -> SnapCapabilities {
        SnapCapabilities {
            window_level: false,
            element_level: false,
            expose_label: false,
        }
    }

    fn set_excluded_window(&self, _token: Option<SnapExclusionToken>) {}

    fn candidates_at(&self, _point: PhysicalPoint) -> Result<Vec<SnapCandidate>, SnapError> {
        Err(SnapError::Unsupported(
            "Linux snap is a demo skeleton: X11 window querying / Wayland compositor interface is not wired yet"
                .to_string(),
        ))
    }
}

/// A ready-to-use Linux platform exposing the two shared backends.
pub struct LinuxPlatform {
    capture: LinuxCaptureBackend,
    snap: LinuxSnapBackend,
}

impl LinuxPlatform {
    pub fn new() -> Self {
        Self {
            capture: LinuxCaptureBackend,
            snap: LinuxSnapBackend,
        }
    }

    pub fn capture_backend(&self) -> &dyn CaptureBackend {
        &self.capture
    }

    pub fn snap_backend(&self) -> &dyn SnapBackend {
        &self.snap
    }
}
