//! Shared toolbar placement computation.
//!
//! The two frontends use this exact function so placement is bit-identical.
//! All inputs/outputs are physical pixels.

use crate::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};

fn saturating_i64_to_i32(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Why a placement was chosen (vertical strategy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarPlacementReason {
    /// Below the selection.
    Below,
    /// Above the selection (no room below).
    Above,
    /// Inside the selection, near its bottom edge (no room below or above).
    InsideBottom,
    /// Only the clamped default fit after all strategies failed.
    Clamped,
}

/// Result of [`place_toolbar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolbarPlacement {
    pub rect: PhysicalRect,
    pub reason: ToolbarPlacementReason,
}

/// Place a toolbar relative to a selection.
///
/// Strategy:
/// 1. below the selection;
/// 2. if below does not fit in `work_area`, inside the selection near its bottom;
/// 3. if the selection is too small for an inside toolbar, above;
/// 4. otherwise clamp the best-effort rect into `work_area`.
///
/// The rect is horizontally centered on the selection and clamped so it stays
/// fully visible laterally (when the work area is wide enough).
pub fn place_toolbar(
    selection: PhysicalRect,
    toolbar_size: PhysicalSize,
    work_area: PhysicalRect,
    preferred_gap: u32,
) -> ToolbarPlacement {
    let tw = toolbar_size.width as i64;
    let th = toolbar_size.height as i64;
    let gap = preferred_gap as i64;

    let center_x = selection.center().x as i64;
    let base_x = center_x - tw / 2;

    let below = PhysicalRect::new(
        PhysicalPoint::new(
            saturating_i64_to_i32(base_x),
            saturating_i64_to_i32(selection.bottom() as i64 + gap),
        ),
        toolbar_size,
    );
    let above = PhysicalRect::new(
        PhysicalPoint::new(
            saturating_i64_to_i32(base_x),
            saturating_i64_to_i32(selection.top() as i64 - gap - th),
        ),
        toolbar_size,
    );
    let inside = PhysicalRect::new(
        PhysicalPoint::new(
            saturating_i64_to_i32(base_x),
            saturating_i64_to_i32(selection.bottom() as i64 - gap - th),
        ),
        toolbar_size,
    );

    let fits = |r: PhysicalRect| {
        r.origin.x >= work_area.origin.x
            && r.right() <= work_area.right()
            && r.origin.y >= work_area.origin.y
            && r.bottom() <= work_area.bottom()
    };

    let (mut rect, reason) = if fits(below) {
        (below, ToolbarPlacementReason::Below)
    } else if fits(inside) {
        (inside, ToolbarPlacementReason::InsideBottom)
    } else if fits(above) {
        (above, ToolbarPlacementReason::Above)
    } else {
        // No strategy fits; start from below and clamp it into the work area.
        (below, ToolbarPlacementReason::Clamped)
    };

    // Horizontally center while keeping the toolbar laterally visible.
    rect = clamp_horizontal(rect, work_area);

    if reason == ToolbarPlacementReason::Clamped {
        rect = rect.clamp(work_area);
    }

    ToolbarPlacement { rect, reason }
}

fn clamp_horizontal(rect: PhysicalRect, work_area: PhysicalRect) -> PhysicalRect {
    let tw = rect.size.width as i64;
    let wa_w = work_area.size.width as i64;
    let min_x = work_area.origin.x;
    let max_x = if tw >= wa_w {
        min_x
    } else {
        saturating_i64_to_i32(work_area.right() as i64 - tw)
    };
    PhysicalRect::new(
        PhysicalPoint::new(rect.origin.x.clamp(min_x, max_x), rect.origin.y),
        rect.size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
        PhysicalRect::new(PhysicalPoint::new(x, y), PhysicalSize::new(w, h))
    }
    fn s(w: u32, h: u32) -> PhysicalSize {
        PhysicalSize::new(w, h)
    }

    /// A typical single-monitor virtual desktop work area.
    fn work() -> PhysicalRect {
        r(0, 0, 1920, 1040)
    }

    #[test]
    fn below_when_selection_is_in_the_middle() {
        let sel = r(400, 300, 800, 300);
        let p = place_toolbar(sel, s(320, 40), work(), 8);
        assert_eq!(p.reason, ToolbarPlacementReason::Below);
        assert_eq!(p.rect.origin.y, sel.bottom() + 8);
        assert_eq!(p.rect.size, s(320, 40));
    }

    #[test]
    fn inside_when_selection_grazes_the_bottom() {
        let sel = r(400, 900, 800, 100);
        let p = place_toolbar(sel, s(320, 40), work(), 8);
        assert_eq!(p.reason, ToolbarPlacementReason::InsideBottom);
        assert_eq!(p.rect.bottom(), sel.bottom() - 8);
    }

    #[test]
    fn inside_bottom_when_selection_spans_the_work_area() {
        // Full-screen-ish selection: no room below or above inside work area.
        let sel = r(0, 0, 1920, 1040);
        let p = place_toolbar(sel, s(320, 40), work(), 8);
        assert_eq!(p.reason, ToolbarPlacementReason::InsideBottom);
        assert_eq!(p.rect.bottom(), sel.bottom() - 8);
    }

    #[test]
    fn tiny_selection_places_below() {
        let sel = r(960, 500, 2, 2);
        let p = place_toolbar(sel, s(320, 40), work(), 8);
        assert_eq!(p.reason, ToolbarPlacementReason::Below);
        assert!(p
            .rect
            .contains_exclusive(PhysicalPoint::new(p.rect.origin.x + 1, p.rect.origin.y + 1)));
    }

    #[test]
    fn negative_monitor_origin_keeps_placement_inside_work_area() {
        // Secondary monitor to the left of primary: origin is negative.
        let wa = r(-1920, 0, 1920, 1040);
        let sel = r(-1600, 400, 600, 200);
        let p = place_toolbar(sel, s(320, 40), wa, 8);
        assert_eq!(p.reason, ToolbarPlacementReason::Below);
        assert!(p.rect.origin.x >= wa.origin.x);
        assert!(p.rect.right() <= wa.right());
        assert!(p.rect.bottom() <= wa.bottom());
    }

    #[test]
    fn clamped_to_work_area_when_toolbar_bigger_than_space() {
        // Tiny work area forces the clamped path.
        let wa = r(0, 0, 100, 100);
        let sel = r(0, 0, 100, 100);
        let p = place_toolbar(sel, s(320, 40), wa, 8);
        assert_eq!(p.reason, ToolbarPlacementReason::Clamped);
        // The result must not escape the work area.
        assert!(p.rect.origin.x >= wa.origin.x);
        assert!(p.rect.right() <= wa.right());
    }

    #[test]
    fn horizontal_centering_clamped_at_left_edge() {
        let wa = r(0, 0, 400, 300);
        let sel = r(0, 100, 10, 10);
        let p = place_toolbar(sel, s(320, 40), wa, 8);
        assert!(p.rect.origin.x >= wa.origin.x);
        assert_eq!(p.rect.origin.x, 0);
    }

    #[test]
    fn property_always_within_work_area() {
        // Sweep a range of selections and assert the placement never leaves the
        // work area (a poor-man's property test).
        let wa = r(0, 0, 1920, 1040);
        for y in (0..1040).step_by(120) {
            for h in [2u32, 10, 100, 300, 1040] {
                let sel = r(300, y, 600, h);
                let p = place_toolbar(sel, s(320, 40), wa, 8);
                assert!(
                    p.rect.origin.x >= wa.origin.x,
                    "x overflow at y={y} h={h}: {:?}",
                    p.rect
                );
                assert!(p.rect.right() <= wa.right());
                assert!(p.rect.origin.y >= wa.origin.y);
                assert!(p.rect.bottom() <= wa.bottom());
            }
        }
    }
}
