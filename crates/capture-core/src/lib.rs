//! `capture-core` — the UI-neutral, platform-neutral domain core.
//!
//! This crate is the foundation of the screenshotting demo. It contains:
//!
//! - `geometry` — physical-pixel geometry primitives (negative-coordinate safe).
//! - `coord` — physical↔logical coordinate mapping for mixed-DPI.
//! - `capture` — [`CapturedFrame`], [`MonitorInfo`], pixel formats, timing.
//! - `snap` — window-adhesion candidates and ranking.
//! - `placement` — shared toolbar placement.
//! - `selection` — pure selection-geometry state.
//!
//! It deliberately has **no** dependency on any UI toolkit or platform API.

pub mod action;
pub mod capture;
pub mod coord;
pub mod geometry;
pub mod placement;
pub mod selection;
pub mod snap;

pub use action::ActionId;
pub use capture::{
    CaptureCapabilities, CaptureError, CapturedFrame, MonitorId, MonitorInfo, PixelFormat, Timing,
};
pub use coord::{CoordinateMapper, VirtualDesktopMapper};
pub use geometry::{
    LogicalPoint, LogicalRect, LogicalSize, PhysicalPoint, PhysicalRect, PhysicalSize, ScaleFactor,
};
pub use placement::{place_toolbar, ToolbarPlacement, ToolbarPlacementReason};
pub use selection::{ResizeHandle, SelectionInteraction, SelectionSession};
pub use snap::{
    rank_candidates, SnapCandidate, SnapCandidateId, SnapCapabilities, SnapError,
    SnapExclusionToken, SnapKind,
};

/// Version banner printed by the CLI / used by tests to confirm the Core build.
pub const CORE_API_VERSION: &str = env!("CARGO_PKG_VERSION");
