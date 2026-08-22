//! `capture-platform-api` — the formal abstraction surface between Core and the
//! platform implementations.
//!
//! This crate defines [`CaptureBackend`] and [`SnapBackend`]. It contains no
//! implementation code and exposes only `capture-core` types, so the two
//! frontends can consume any backend through `&dyn`.

use capture_core::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PhysicalPoint,
    SnapCandidate, SnapCapabilities, SnapError, SnapExclusionToken,
};

/// A backend that can enumerate monitors and capture their pixels.
///
/// Implementations must return physical-pixel frames whose origin matches the
/// monitor's virtual-desktop bounding rect. Instances must be usable from a
/// worker thread (`Send + Sync`).
pub trait CaptureBackend: Send + Sync {
    fn capabilities(&self) -> CaptureCapabilities;

    /// Enumerate all connected monitors. Order is stable within a run.
    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;

    /// Capture the current contents of the given monitor as a physical-pixel
    /// [`CapturedFrame`].
    fn capture_monitor(&self, id: MonitorId) -> Result<CapturedFrame, CaptureError>;

    /// Capture one atomic physical-pixel frame of the complete virtual desktop.
    ///
    /// Backends that cannot provide this operation should return
    /// [`CaptureError::Unsupported`]. A frontend can use this operation for a
    /// cross-monitor frozen overlay instead of stitching independently captured
    /// monitor frames.
    fn capture_virtual_desktop(&self) -> Result<CapturedFrame, CaptureError> {
        Err(CaptureError::Unsupported(
            "virtual-desktop capture is not available on this backend".to_string(),
        ))
    }
}

/// A backend that can find windows/elements under a point for snap-to-window.
///
/// The screenshotter must exclude its own overlay and Pin windows via
/// [`SnapBackend::set_excluded_windows`] before snapping so it never highlights
/// itself.
pub trait SnapBackend: Send + Sync {
    fn capabilities(&self) -> SnapCapabilities;

    /// Return the candidates under `point`, best-first (topmost candidate first).
    fn candidates_at(&self, point: PhysicalPoint) -> Result<Vec<SnapCandidate>, SnapError>;

    /// Set the opaque identity of a window to exclude from snapping. Re-entrant
    /// and thread-safe: notifies implementors, default is a no-op.
    fn set_excluded_window(&self, token: Option<SnapExclusionToken>) {
        let tokens = token.into_iter().collect::<Vec<_>>();
        self.set_excluded_windows(&tokens);
    }

    /// Set all native windows that must be excluded from snapping.
    ///
    /// The singular method is retained for compatibility with existing
    /// frontends. Implementations that support multiple overlay/pin windows
    /// should override this method.
    fn set_excluded_windows(&self, tokens: &[SnapExclusionToken]) {
        let _ = tokens;
    }
}
