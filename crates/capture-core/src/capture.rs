//! Capture domain types shared by the Core and both frontends.
//!
//! A [`CapturedFrame`] is the single frame representation every backend produces
//! and every frontend consumes. The Core does not use any OS toolkit image type.

use crate::geometry::{PhysicalPoint, PhysicalRect, ScaleFactor};
use std::sync::Arc;

/// The canonical in-memory pixel layout. The demo normalizes to RGBA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PixelFormat {
    #[default]
    Rgba8,
    Bgra8,
    Rgb24,
    /// 8-bit grayscale (not produced by any backend yet; reserved).
    U8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => 4,
            PixelFormat::Rgb24 => 3,
            PixelFormat::U8 => 1,
        }
    }
}

/// A captured frame in physical pixels.
///
/// `pixels` is a tightly packed row-major buffer of `stride * height` bytes.
/// `origin` is the frame's top-left in virtual-desktop physical coordinates and
/// may be negative.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub pixels: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub origin: PhysicalPoint,
    pub pixel_format: PixelFormat,
}

impl CapturedFrame {
    pub fn new(
        pixels: Arc<[u8]>,
        width: u32,
        height: u32,
        stride: u32,
        origin: PhysicalPoint,
        pixel_format: PixelFormat,
    ) -> Self {
        Self {
            pixels,
            width,
            height,
            stride,
            origin,
            pixel_format,
        }
    }

    /// The required buffer length for this frame's geometry.
    pub fn expected_len(&self) -> usize {
        (self.stride as usize) * (self.height as usize)
    }

    pub fn bounds(&self) -> PhysicalRect {
        PhysicalRect::new(
            self.origin,
            crate::geometry::PhysicalSize::new(self.width, self.height),
        )
    }

    /// Returns a slice of one row, or `None` if `row` is out of range or the
    /// buffer is shorter than expected.
    pub fn row(&self, row: u32) -> Option<&[u8]> {
        if row >= self.height || self.expected_len() > self.pixels.len() {
            return None;
        }
        let start = (row as usize) * (self.stride as usize);
        let end = start + (self.stride as usize);
        self.pixels.get(start..end)
    }

    /// Validate the buffer geometry. Returns the number of bytes the frame needs.
    pub fn validate(&self) -> Result<usize, CaptureError> {
        let needed = self.expected_len();
        if self.pixels.len() < needed {
            return Err(CaptureError::CaptureFailed(format!(
                "frame buffer too small: {} < {}",
                self.pixels.len(),
                needed
            )));
        }
        Ok(needed)
    }

    /// Convert (in place if possible) the pixel data to RGBA8. This is a no-op
    /// copy for Rgba8, and a BGR→RGB channel swap plus alpha fill otherwise.
    pub fn to_rgba8(self) -> CapturedFrame {
        if self.pixel_format == PixelFormat::Rgba8 {
            return self;
        }
        let rgba = match self.pixel_format {
            PixelFormat::Rgba8 => self.pixels,
            PixelFormat::Bgra8 => {
                let out = self
                    .pixels
                    .chunks_exact(4)
                    .map(|c| [c[2], c[1], c[0], c[3]])
                    .flatten()
                    .collect::<Vec<_>>();
                out.into()
            }
            PixelFormat::Rgb24 => {
                let out = self
                    .pixels
                    .chunks_exact(3)
                    .map(|c| [c[0], c[1], c[2], 0xFF])
                    .flatten()
                    .collect::<Vec<_>>();
                out.into()
            }
            PixelFormat::U8 => {
                let out = self
                    .pixels
                    .iter()
                    .flat_map(|&g| [g, g, g, 0xFF])
                    .collect::<Vec<_>>();
                out.into()
            }
        };
        CapturedFrame::new(
            rgba,
            self.width,
            self.height,
            self.width * 4,
            self.origin,
            PixelFormat::Rgba8,
        )
    }
}

/// Opaque, integer-backed identity of a monitor. Never wraps an OS-specific
/// handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorId(pub u32);

impl MonitorId {
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Description of a single display.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    pub name: String,
    pub bounds: PhysicalRect,
    pub scale_factor: ScaleFactor,
    pub is_primary: bool,
}

/// Static capabilities reported by a capture backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureCapabilities {
    pub multi_monitor: bool,
    pub per_monitor_dpi: bool,
    pub capture_window: bool,
    pub live_preview: bool,
}

/// Errors produced by a capture backend or by Core capture logic.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no capture backend available: {0}")]
    BackendUnavailable(String),
    #[error("monitor not found: {0:?}")]
    MonitorNotFound(MonitorId),
    #[error("capture failed: {0}")]
    CaptureFailed(String),
    #[error("operation not supported on this platform: {0}")]
    Unsupported(String),
    #[error("unsupported pixel format: {0:?}")]
    FormatUnsupported(PixelFormat),
}

/// Performance beats surfaced by the Core. `T0` is the hotkey, `T1` is the first
/// captured frame. Frontends add `T2`/`T3`/`T4`.
#[derive(Debug, Clone, Default)]
pub struct Timing {
    pub t0_hotkey_received: Option<std::time::Instant>,
    pub t1_frame_ready: Option<std::time::Instant>,
}

impl Timing {
    pub fn mark_t0(&mut self) {
        self.t0_hotkey_received = Some(std::time::Instant::now());
    }

    pub fn mark_t1(&mut self) {
        self.t1_frame_ready = Some(std::time::Instant::now());
    }

    /// `capture latency = T1 - T0`.
    pub fn capture_latency(&self) -> Option<std::time::Duration> {
        match (self.t0_hotkey_received, self.t1_frame_ready) {
            (Some(t0), Some(t1)) => Some(t1.duration_since(t0)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_frame(w: u32, h: u32) -> CapturedFrame {
        let bytes = (w * h * 4) as usize;
        CapturedFrame::new(
            vec![0xABu8; bytes].into(),
            w,
            h,
            w * 4,
            PhysicalPoint::new(0, 0),
            PixelFormat::Rgba8,
        )
    }

    #[test]
    fn row_slices_are_correct() {
        let f = rgba_frame(4, 2);
        assert_eq!(f.row(0).unwrap().len(), 16);
        assert_eq!(f.row(1).unwrap().len(), 16);
        assert!(f.row(2).is_none());
    }

    #[test]
    fn validate_rejects_short_buffer() {
        let f = CapturedFrame::new(
            vec![0u8; 8].into(),
            10,
            10,
            40,
            PhysicalPoint::new(0, 0),
            PixelFormat::Rgba8,
        );
        assert!(f.validate().is_err());
    }

    #[test]
    fn bgra_to_rgba_swaps_channels() {
        // BGRA byte 0,1,2 = B,G,R,A ; RGBA should be R,G,B,A.
        let px = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        let f = CapturedFrame::new(
            px.into(),
            2,
            1,
            8,
            PhysicalPoint::new(0, 0),
            PixelFormat::Bgra8,
        );
        let rgba = f.to_rgba8();
        assert_eq!(&rgba.pixels[0..4], &[30, 20, 10, 40]);
        assert_eq!(&rgba.pixels[4..8], &[70, 60, 50, 80]);
        assert_eq!(rgba.pixel_format, PixelFormat::Rgba8);
        assert_eq!(rgba.stride, 8);
    }
}
