//! `capture-linux` — Linux X11/Wayland capture and EWMH window snapping.
//!
//! X11 is implemented with RandR monitor enumeration, root-window `GetImage`
//! capture, and EWMH client-list/window geometry queries. Native Wayland uses
//! the XDG ScreenCast portal and a short-lived PipeWire consumer for an
//! authorized frame. Pure Wayland has no global window-list protocol, so
//! window-level snap remains unavailable there.

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::{native_wayland_selected, LinuxPlatform};

#[cfg(not(target_os = "linux"))]
pub mod non_linux {
    //! Marker module: this crate provides a Linux backend; it is empty elsewhere.
}
