//! `capture-linux` — Linux (X11 / Wayland) capture + snap backend skeleton.
//!
//! For the demo this provides a **compilable backend route**: the two traits are
//! implemented and report `Unsupported` with a clear message, so a frontend can
//! wire `capture-linux` and get a deterministic "not implemented on this
//! platform" result rather than a crash. The production capture path (X11 via
//! the XRandR / XShm route, Wayland via the XDG ScreenCast portal) is documented
//! in `CORE_BASELINE_REPORT.md` and left as follow-up work behind the same API.

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform;

#[cfg(not(target_os = "linux"))]
pub mod non_linux {
    //! Marker module: this crate provides a Linux backend; it is empty elsewhere.
}
