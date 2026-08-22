//! `capture-windows` — Windows capture + snap backend (see ADR-0001 / ADR-0002).
//!
//! On non-Windows targets this crate compiles to an empty crate so the workspace
//! builds everywhere.

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod platform;
#[cfg(windows)]
pub mod snap;

#[cfg(windows)]
pub use platform::WindowsPlatform;

#[cfg(not(windows))]
pub mod non_windows {
    //! Marker module: this crate is Windows-only.
}
