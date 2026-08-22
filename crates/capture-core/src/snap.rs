//! Snap (window-adhesion) domain types shared by Core and frontends.

use crate::geometry::{PhysicalPoint, PhysicalRect};

/// The kind of a snap candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapKind {
    /// A UI Automation element inside a top-level window.
    Element,
    /// A top-level window.
    Window,
    /// A bounding region of the virtual desktop (fallback / catch-all).
    Desktop,
}

/// Opaque identity of a snap candidate (a window handle value on Windows, an X11
/// window id or Wayland surface id on Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapCandidateId(pub u64);

impl SnapCandidateId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A candidate the user can snap the selection to.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapCandidate {
    pub id: SnapCandidateId,
    pub bounds: PhysicalRect,
    pub kind: SnapKind,
    pub label: Option<String>,
    /// Lower values are closer to the top of the platform's z-order.
    /// `u32::MAX` is reserved for candidates without a platform z-order, such
    /// as the desktop fallback.
    pub z_order: u32,
}

/// Static capabilities of a snap backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapCapabilities {
    pub window_level: bool,
    pub element_level: bool,
    pub expose_label: bool,
}

/// Opaque token identifying a window (e.g. the screenshotter's own overlay) to
/// exclude from snapping. The value is backend-defined; the Core only passes it
/// through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapExclusionToken(pub u64);

impl SnapExclusionToken {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Errors produced by a snap backend.
#[derive(Debug, thiserror::Error)]
pub enum SnapError {
    #[error("snap backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("snap failed: {0}")]
    SnapFailed(String),
    #[error("operation not supported on this platform: {0}")]
    Unsupported(String),
}

/// Rank candidates for a given point. The order returned is the order the UI
/// should prefer (topmost first). A pure function so it can be unit-tested
/// without a real backend.
pub fn rank_candidates(
    point: PhysicalPoint,
    mut candidates: Vec<SnapCandidate>,
) -> Vec<SnapCandidate> {
    // Prefer candidates containing the point, then windows over the desktop,
    // then the platform-provided z-order. Area is only a deterministic fallback
    // when two candidates have the same z-order.
    candidates.sort_by(|a, b| {
        let a_hit = a.bounds.contains_exclusive(point) as u8;
        let b_hit = b.bounds.contains_exclusive(point) as u8;
        let kind_rank = |kind: SnapKind| match kind {
            SnapKind::Element => 2,
            SnapKind::Window => 1,
            SnapKind::Desktop => 0,
        };
        let a_kind = kind_rank(a.kind);
        let b_kind = kind_rank(b.kind);
        let a_area = a.bounds.area();
        let b_area = b.bounds.area();
        b_hit
            .cmp(&a_hit)
            .then(b_kind.cmp(&a_kind))
            .then(a.z_order.cmp(&b.z_order))
            .then(b_area.cmp(&a_area))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(cx: i32, cy: i32, w: u32, h: u32, label: &str) -> SnapCandidate {
        SnapCandidate {
            id: SnapCandidateId::new(1),
            bounds: PhysicalRect::new(
                PhysicalPoint::new(cx, cy),
                crate::geometry::PhysicalSize::new(w, h),
            ),
            kind: SnapKind::Window,
            label: Some(label.to_string()),
            z_order: 0,
        }
    }

    #[test]
    fn ranking_prefers_containing_window() {
        let point = PhysicalPoint::new(100, 100);
        let window_inside = win(50, 50, 200, 200, "inside");
        let window_outside = win(500, 500, 200, 200, "outside");

        let ranked = rank_candidates(point, vec![window_outside.clone(), window_inside.clone()]);
        assert_eq!(ranked[0].label, Some("inside".to_string()));
    }

    #[test]
    fn larger_window_wins_ties() {
        let point = PhysicalPoint::new(100, 100);
        let big = win(0, 0, 300, 300, "big");
        let small = win(0, 0, 150, 150, "small");
        let ranked = rank_candidates(point, vec![small, big.clone()]);
        assert_eq!(ranked[0].label, Some("big".to_string()));
    }

    #[test]
    fn desktop_candidate_ranks_last() {
        let point = PhysicalPoint::new(10, 10);
        let desktop = SnapCandidate {
            id: SnapCandidateId::new(0),
            bounds: PhysicalRect::new(
                PhysicalPoint::new(0, 0),
                crate::geometry::PhysicalSize::new(3000, 2000),
            ),
            kind: SnapKind::Desktop,
            label: None,
            z_order: u32::MAX,
        };
        let window = win(0, 0, 100, 100, "w");
        let ranked = rank_candidates(point, vec![desktop, window.clone()]);
        assert_eq!(ranked[0].label, Some("w".to_string()));
    }

    #[test]
    fn element_candidate_ranks_above_its_window() {
        let point = PhysicalPoint::new(100, 100);
        let element = SnapCandidate {
            id: SnapCandidateId::new(2),
            bounds: PhysicalRect::new(
                PhysicalPoint::new(20, 20),
                crate::geometry::PhysicalSize::new(160, 160),
            ),
            kind: SnapKind::Element,
            label: Some("button".to_string()),
            z_order: 0,
        };
        let window = win(0, 0, 300, 300, "window");
        let ranked = rank_candidates(point, vec![window, element]);
        assert_eq!(ranked[0].kind, SnapKind::Element);
    }

    #[test]
    fn platform_z_order_beats_area_when_windows_overlap() {
        let point = PhysicalPoint::new(100, 100);
        let mut top = win(0, 0, 200, 200, "top");
        top.z_order = 2;
        let mut underneath = win(0, 0, 300, 300, "underneath");
        underneath.z_order = 8;
        let ranked = rank_candidates(point, vec![underneath, top]);
        assert_eq!(ranked[0].label.as_deref(), Some("top"));
    }
}
