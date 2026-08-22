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
        (self.stride as usize).saturating_mul(self.height as usize)
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
        if row >= self.height {
            return None;
        }
        let start = (row as usize).checked_mul(self.stride as usize)?;
        let end = start.checked_add(self.stride as usize)?;
        if end > self.pixels.len() {
            return None;
        }
        self.pixels.get(start..end)
    }

    /// Validate the buffer geometry and pixel-format stride contract.
    /// Returns the number of bytes required by the declared stride and height.
    pub fn validate(&self) -> Result<usize, CaptureError> {
        if self.width == 0 || self.height == 0 {
            return Err(CaptureError::InvalidFrame(
                "frame dimensions must be non-zero".to_string(),
            ));
        }
        let minimum_stride = self
            .width
            .checked_mul(self.pixel_format.bytes_per_pixel())
            .ok_or_else(|| CaptureError::InvalidFrame("frame stride overflows u32".to_string()))?;
        if self.stride < minimum_stride {
            return Err(CaptureError::InvalidFrame(format!(
                "frame stride too small: {} < {}",
                self.stride, minimum_stride
            )));
        }
        let needed = (self.stride as usize)
            .checked_mul(self.height as usize)
            .ok_or_else(|| {
                CaptureError::InvalidFrame("frame buffer length overflows usize".to_string())
            })?;
        if self.pixels.len() < needed {
            return Err(CaptureError::InvalidFrame(format!(
                "frame buffer too small: {} < {}",
                self.pixels.len(),
                needed
            )));
        }
        Ok(needed)
    }

    /// Convert the frame to tightly packed RGBA8.
    ///
    /// Row padding is removed during conversion, so callers can rely on a
    /// `width * 4` stride after this method. Invalid geometry or pixel data is
    /// returned as a structured [`CaptureError`].
    pub fn to_rgba8(self) -> Result<CapturedFrame, CaptureError> {
        self.validate()?;
        let output_stride = self
            .width
            .checked_mul(4)
            .ok_or_else(|| CaptureError::InvalidFrame("RGBA stride overflows u32".to_string()))?;
        if self.pixel_format == PixelFormat::Rgba8 && self.stride == output_stride {
            return Ok(self);
        }
        let output_len = (output_stride as usize)
            .checked_mul(self.height as usize)
            .ok_or_else(|| {
                CaptureError::InvalidFrame("RGBA buffer length overflows usize".to_string())
            })?;
        let mut rgba = vec![0u8; output_len];
        let source_bpp = self.pixel_format.bytes_per_pixel() as usize;
        for y in 0..self.height as usize {
            let source_start = y * self.stride as usize;
            let source_row = self
                .pixels
                .get(source_start..source_start + self.stride as usize)
                .ok_or_else(|| {
                    CaptureError::InvalidFrame("frame row is unavailable".to_string())
                })?;
            for x in 0..self.width as usize {
                let source_index = x * source_bpp;
                let destination_index = (y * output_stride as usize) + x * 4;
                let pixel = source_row
                    .get(source_index..source_index + source_bpp)
                    .ok_or_else(|| {
                        CaptureError::InvalidFrame("frame pixel is unavailable".to_string())
                    })?;
                match self.pixel_format {
                    PixelFormat::Rgba8 => {
                        rgba[destination_index..destination_index + 4].copy_from_slice(pixel)
                    }
                    PixelFormat::Bgra8 => {
                        rgba[destination_index..destination_index + 4]
                            .copy_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                    }
                    PixelFormat::Rgb24 => {
                        rgba[destination_index..destination_index + 4]
                            .copy_from_slice(&[pixel[0], pixel[1], pixel[2], 0xFF]);
                    }
                    PixelFormat::U8 => {
                        rgba[destination_index..destination_index + 4]
                            .copy_from_slice(&[pixel[0], pixel[0], pixel[0], 0xFF]);
                    }
                }
            }
        }
        Ok(CapturedFrame::new(
            rgba.into(),
            self.width,
            self.height,
            output_stride,
            self.origin,
            PixelFormat::Rgba8,
        ))
    }

    /// Crop a frame to a rectangle in virtual-desktop coordinates.
    pub fn crop(&self, rect: PhysicalRect) -> Result<CapturedFrame, CaptureError> {
        self.validate()?;
        let bounds = self.bounds();
        if rect.is_empty()
            || rect.origin.x < bounds.origin.x
            || rect.origin.y < bounds.origin.y
            || rect.right() > bounds.right()
            || rect.bottom() > bounds.bottom()
        {
            return Err(CaptureError::InvalidFrame(
                "crop rectangle lies outside the frame".to_string(),
            ));
        }
        let rgba = self.clone().to_rgba8()?;
        let output_stride = rect
            .width()
            .checked_mul(4)
            .ok_or_else(|| CaptureError::InvalidFrame("crop stride overflows u32".to_string()))?;
        let output_len = (output_stride as usize)
            .checked_mul(rect.height() as usize)
            .ok_or_else(|| {
                CaptureError::InvalidFrame("crop buffer length overflows usize".to_string())
            })?;
        let mut pixels = vec![0u8; output_len];
        let offset_x = usize::try_from(rect.origin.x as i64 - rgba.origin.x as i64)
            .map_err(|_| CaptureError::InvalidFrame("crop x offset is negative".to_string()))?;
        let offset_y = usize::try_from(rect.origin.y as i64 - rgba.origin.y as i64)
            .map_err(|_| CaptureError::InvalidFrame("crop y offset is negative".to_string()))?;
        for y in 0..rect.height() as usize {
            let row_index = offset_y.checked_add(y).ok_or_else(|| {
                CaptureError::InvalidFrame("crop row index overflows usize".to_string())
            })?;
            let source_start = row_index
                .checked_mul(rgba.stride as usize)
                .and_then(|row| row.checked_add(offset_x.checked_mul(4)?))
                .ok_or_else(|| {
                    CaptureError::InvalidFrame("crop source index overflows usize".to_string())
                })?;
            let destination_start = y * output_stride as usize;
            let count = output_stride as usize;
            let source_end = source_start.checked_add(count).ok_or_else(|| {
                CaptureError::InvalidFrame("crop source row overflows usize".to_string())
            })?;
            let destination_end = destination_start.checked_add(count).ok_or_else(|| {
                CaptureError::InvalidFrame("crop destination row overflows usize".to_string())
            })?;
            let source_row = rgba.pixels.get(source_start..source_end).ok_or_else(|| {
                CaptureError::InvalidFrame("crop source row is unavailable".to_string())
            })?;
            pixels[destination_start..destination_end].copy_from_slice(source_row);
        }
        Ok(CapturedFrame::new(
            pixels.into(),
            rect.width(),
            rect.height(),
            output_stride,
            rect.origin,
            PixelFormat::Rgba8,
        ))
    }
}

/// Opaque, integer-backed identity of a monitor. Never wraps an OS-specific
/// handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorId(pub u64);

impl MonitorId {
    /// Construct an ID from a raw stable value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Derive a stable ID from a platform monitor key such as a Windows device
    /// name. FNV-1a is deterministic and sufficient for this opaque demo ID.
    pub fn from_stable_key(key: &str) -> Self {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in key.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self(hash)
    }

    pub const fn index(self) -> u64 {
        self.0
    }
}

/// Description of a single display.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    pub name: String,
    pub bounds: PhysicalRect,
    pub work_area: PhysicalRect,
    pub scale_factor: ScaleFactor,
    pub is_primary: bool,
}

/// Static capabilities reported by a capture backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureCapabilities {
    pub multi_monitor: bool,
    pub per_monitor_dpi: bool,
    pub capture_virtual_desktop: bool,
    pub capture_window: bool,
    pub live_preview: bool,
}

/// Errors produced by a capture backend or by Core capture logic.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CaptureError {
    #[error("no capture backend available: {0}")]
    BackendUnavailable(String),
    #[error("monitor not found: {0:?}")]
    MonitorNotFound(MonitorId),
    #[error("capture failed: {0}")]
    CaptureFailed(String),
    #[error("invalid capture frame: {0}")]
    InvalidFrame(String),
    #[error("invalid selection: {0}")]
    InvalidSelection(String),
    #[error("invalid annotation: {0}")]
    InvalidAnnotation(String),
    #[error("invalid session command: {0}")]
    InvalidCommand(String),
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
    pub fn reset(&mut self) {
        self.t0_hotkey_received = None;
        self.t1_frame_ready = None;
    }

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
        let rgba = f.to_rgba8().unwrap();
        assert_eq!(&rgba.pixels[0..4], &[30, 20, 10, 40]);
        assert_eq!(&rgba.pixels[4..8], &[70, 60, 50, 80]);
        assert_eq!(rgba.pixel_format, PixelFormat::Rgba8);
        assert_eq!(rgba.stride, 8);
    }

    #[test]
    fn conversion_drops_row_padding_and_supports_rgb24() {
        let f = CapturedFrame::new(
            vec![
                1, 2, 3, 4, 5, 6, 99, 99, // row 0, two bytes of padding
                7, 8, 9, 10, 11, 12, 88, 88,
            ]
            .into(),
            2,
            2,
            8,
            PhysicalPoint::new(0, 0),
            PixelFormat::Rgb24,
        );
        let rgba = f.to_rgba8().unwrap();
        assert_eq!(rgba.stride, 8);
        assert_eq!(
            &rgba.pixels[..],
            &[1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }

    #[test]
    fn conversion_expands_grayscale_to_opaque_rgba() {
        let f = CapturedFrame::new(
            vec![10, 20, 99, 88].into(),
            2,
            1,
            4,
            PhysicalPoint::new(0, 0),
            PixelFormat::U8,
        );
        let rgba = f.to_rgba8().unwrap();
        assert_eq!(&rgba.pixels[..], &[10, 10, 10, 255, 20, 20, 20, 255]);
    }

    #[test]
    fn crop_preserves_negative_virtual_desktop_origin() {
        let mut f = rgba_frame(4, 3);
        f.origin = PhysicalPoint::new(-10, -20);
        let cropped = f
            .crop(PhysicalRect::new(
                PhysicalPoint::new(-9, -19),
                crate::geometry::PhysicalSize::new(2, 2),
            ))
            .unwrap();
        assert_eq!(cropped.origin, PhysicalPoint::new(-9, -19));
        assert_eq!(cropped.width, 2);
        assert_eq!(cropped.height, 2);
        assert_eq!(cropped.stride, 8);
    }
}
