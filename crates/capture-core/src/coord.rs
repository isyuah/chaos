//! Coordinate mapping between physical pixels (Core canonical) and logical
//! pixels (toolkit UI space), for mixed-DPI support.

use crate::capture::{MonitorId, MonitorInfo};
use crate::geometry::{
    LogicalPoint, LogicalRect, LogicalSize, PhysicalPoint, PhysicalRect, ScaleFactor,
};

fn saturating_i64_to_i32(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn logical_length_to_physical(value: f32, scale: f64) -> u32 {
    let scaled = value as f64 * scale;
    if !scaled.is_finite() || scaled <= 0.0 {
        0
    } else {
        scaled.round().min(u32::MAX as f64) as u32
    }
}

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
        let local_x = (p.x as i64 - self.physical_origin.x as i64) as f64;
        let local_y = (p.y as i64 - self.physical_origin.y as i64) as f64;
        LogicalPoint::new((local_x / s) as f32, (local_y / s) as f32)
    }

    /// Convert a toolkit logical point back to a physical point.
    pub fn logical_to_physical(&self, p: LogicalPoint) -> PhysicalPoint {
        let s = self.scale_factor.sanitized();
        let local_x = if p.x.is_finite() {
            (p.x as f64 * s).round()
        } else {
            0.0
        };
        let local_y = if p.y.is_finite() {
            (p.y as f64 * s).round()
        } else {
            0.0
        };
        PhysicalPoint::new(
            saturating_i64_to_i32((local_x as i64).saturating_add(self.physical_origin.x as i64)),
            saturating_i64_to_i32((local_y as i64).saturating_add(self.physical_origin.y as i64)),
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
                logical_length_to_physical(rect.size.width, self.scale_factor.sanitized()),
                logical_length_to_physical(rect.size.height, self.scale_factor.sanitized()),
            ),
        )
    }
}

/// A mixed-DPI virtual desktop split into per-monitor logical surfaces.
///
/// There is no single continuous logical coordinate space when neighboring
/// monitors use different scale factors. This type therefore maps a physical
/// point to the monitor-local logical surface and splits a physical rectangle
/// at monitor boundaries. Physical coordinates remain the source of truth for
/// capture, selection, and rendering.
#[derive(Debug, Clone, Default)]
pub struct VirtualDesktopMapper {
    monitors: Vec<(MonitorId, PhysicalRect, CoordinateMapper)>,
}

impl VirtualDesktopMapper {
    pub fn new(monitors: &[MonitorInfo]) -> Self {
        Self {
            monitors: monitors
                .iter()
                .map(|monitor| {
                    (
                        monitor.id,
                        monitor.bounds,
                        CoordinateMapper::new(monitor.scale_factor, monitor.bounds.origin),
                    )
                })
                .collect(),
        }
    }

    /// Return the monitor-local logical point containing a physical point.
    pub fn physical_to_logical(&self, point: PhysicalPoint) -> Option<(MonitorId, LogicalPoint)> {
        self.monitors
            .iter()
            .find(|(_, bounds, _)| bounds.contains_exclusive(point))
            .map(|(id, _, mapper)| (*id, mapper.physical_to_logical(point)))
    }

    /// Convert a monitor-local logical point back into global physical space.
    pub fn logical_to_physical(
        &self,
        monitor_id: MonitorId,
        point: LogicalPoint,
    ) -> Option<PhysicalPoint> {
        self.monitors
            .iter()
            .find(|(id, _, _)| *id == monitor_id)
            .map(|(_, _, mapper)| mapper.logical_to_physical(point))
    }

    /// Split a physical rect into monitor-local segments.
    pub fn physical_rect_segments(
        &self,
        rect: PhysicalRect,
    ) -> Vec<(MonitorId, PhysicalRect, LogicalRect)> {
        self.monitors
            .iter()
            .filter_map(|(id, bounds, mapper)| {
                let segment = rect.intersection(*bounds)?;
                Some((*id, segment, mapper.physical_rect_to_logical(segment)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::PhysicalSize;

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

    #[test]
    fn extreme_coordinates_do_not_wrap() {
        let m = CoordinateMapper::new(ScaleFactor::new(2.0), PhysicalPoint::new(i32::MIN, 0));
        let point = m.logical_to_physical(LogicalPoint::new(f32::MAX, f32::NAN));
        assert_eq!(point.x, i32::MAX);
        assert_eq!(point.y, 0);
    }

    #[test]
    fn invalid_logical_size_collapses_to_zero() {
        let m = CoordinateMapper::new(ScaleFactor::new(1.5), PhysicalPoint::ZERO);
        let physical = m.logical_rect_to_physical(LogicalRect::new(
            LogicalPoint::new(0.0, 0.0),
            LogicalSize::new(f32::NAN, -10.0),
        ));
        assert_eq!(physical.size, crate::geometry::PhysicalSize::ZERO);
    }

    #[test]
    fn virtual_desktop_mapping_splits_across_mixed_dpi_monitors() {
        let monitors = [
            MonitorInfo {
                id: MonitorId::new(1),
                name: "left".to_string(),
                bounds: PhysicalRect::new(
                    PhysicalPoint::new(-1920, 0),
                    PhysicalSize::new(1920, 1080),
                ),
                work_area: PhysicalRect::new(
                    PhysicalPoint::new(-1920, 0),
                    PhysicalSize::new(1920, 1040),
                ),
                scale_factor: ScaleFactor::new(1.5),
                is_primary: false,
            },
            MonitorInfo {
                id: MonitorId::new(2),
                name: "primary".to_string(),
                bounds: PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(1920, 1080)),
                work_area: PhysicalRect::new(
                    PhysicalPoint::new(0, 0),
                    PhysicalSize::new(1920, 1040),
                ),
                scale_factor: ScaleFactor::new(1.0),
                is_primary: true,
            },
        ];
        let mapper = VirtualDesktopMapper::new(&monitors);
        let (monitor_id, logical) = mapper
            .physical_to_logical(PhysicalPoint::new(-1800, 100))
            .expect("point should be on the left monitor");
        assert_eq!(monitor_id, MonitorId::new(1));
        assert!((logical.x - 80.0).abs() < 0.001);
        assert!((logical.y - 66.66667).abs() < 0.001);
        let segments = mapper.physical_rect_segments(PhysicalRect::new(
            PhysicalPoint::new(-100, 10),
            PhysicalSize::new(200, 20),
        ));
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].0, MonitorId::new(1));
        assert_eq!(segments[1].0, MonitorId::new(2));
    }
}
