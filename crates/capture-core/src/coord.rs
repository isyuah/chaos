//! Coordinate mapping between physical pixels (Core canonical) and logical
//! pixels (toolkit UI space), for mixed-DPI support.

use crate::geometry::{LogicalPoint, LogicalRect, LogicalSize, PhysicalPoint, PhysicalRect, ScaleFactor};

/// Per-monitor mapping between physical and logical coordinates.
///
/// `physical_origin` is this monitor's top-left in virtual-desktop physical
/// coordinates (used to keep logical coordinates local to the monitor).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CoordinateMapper {
    pub scale_factor: ScaleFactor,
    pub physical_origin: PhysicalPoint,
}

impl CoordinateMapper {
    pub const fn new(scale_factor: ScaleFactor, physical_origin: PhysicalPoint) -> Self {
        Self {
            scale_factor,
            physical_origin,
        }
    }

    /// Convert a physical point to the toolkit's logical point for this monitor.
    pub fn physical_to_logical(&self, p: PhysicalPoint) -> LogicalPoint {
        let s = self.scale_factor.sanitized();
        let local_x = (p.x - self.physical_origin.x) as f64;
        let local_y = (p.y - self.physical_origin.y) as f64;
        LogicalPoint::new((local_x / s) as f32, (local_y / s) as f32)
    }

    /// Convert a toolkit logical point back to a physical point.
    pub fn logical_to_physical(&self, p: LogicalPoint) -> PhysicalPoint {
        let s = self.scale_factor.sanitized();
        PhysicalPoint::new(
            (p.x as f64 * s).round() as i32 + self.physical_origin.x,
            (p.y as f64 * s).round() as i32 + self.physical_origin.y,
        )
    }

    pub fn physical_size_to_logical(&self, size: crate::geometry::PhysicalSize) -> LogicalSize {
        let s = self.scale_factor.sanitized();
        LogicalSize::new(
            (size.width as f64 / s) as f32,
            (size.height as f64 / s) as f32,
        )
    }

    pub fn physical_rect_to_logical(&self, rect: PhysicalRect) -> LogicalRect {
        LogicalRect {
            origin: self.physical_to_logical(rect.origin),
            size: self.physical_size_to_logical(rect.size),
        }
    }

    pub fn logical_rect_to_physical(&self, rect: LogicalRect) -> PhysicalRect {
        PhysicalRect::new(
            self.logical_to_physical(rect.origin),
            crate::geometry::PhysicalSize::new(
                (rect.size.width as f64 * self.scale_factor.sanitized()).round() as u32,
                (rect.size.height as f64 * self.scale_factor.sanitized()).round() as u32,
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_to_logical_round_trips() {
        let m = CoordinateMapper::new(ScaleFactor::new(1.25), PhysicalPoint::new(-1920, 0));
        let phys = PhysicalPoint::new(-1920 + 100, 200);
        let l = m.physical_to_logical(phys);
        assert_eq!(l.x, 80.0);
        assert_eq!(l.y, 160.0);
        assert_eq!(m.logical_to_physical(l), phys);
    }

    #[test]
    fn logical_origin_hosted_on_monitor() {
        let m = CoordinateMapper::new(ScaleFactor::new(1.5), PhysicalPoint::new(100, 100));
        let l = m.physical_to_logical(PhysicalPoint::new(100, 100));
        assert_eq!(l, LogicalPoint::new(0.0, 0.0));
    }

    #[test]
    fn negative_origin_maps_into_local_logical_space() {
        let m = CoordinateMapper::new(ScaleFactor::new(2.0), PhysicalPoint::new(-1920, -100));
        let l = m.physical_to_logical(PhysicalPoint::new(-1920, -100));
        assert_eq!(l.x, 0.0);
        assert_eq!(l.y, 0.0);
    }
}
