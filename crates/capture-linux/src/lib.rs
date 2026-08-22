//! `capture-linux` — Linux X11 capture and EWMH window snapping.
//!
//! X11 is implemented with RandR monitor enumeration, root-window `GetImage`
//! capture, and EWMH client-list/window geometry queries. Wayland is detected
//! explicitly and returns an actionable `Unsupported` error because a portal
//! capture requires user permission and a frontend-owned PipeWire consumer.

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform;

#[cfg(not(target_os = "linux"))]
pub mod non_linux {
    //! Marker module: this crate provides a Linux backend; it is empty elsewhere.
}
