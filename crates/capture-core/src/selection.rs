//! Pure selection-geometry state.
//!
//! This is the selection *domain* (drag, move, resize handles) captured in Core.
//! It contains no annotation document — that lives in `capture-annotation`.

use crate::geometry::{PhysicalPoint, PhysicalRect};

/// The eight resize handles of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl ResizeHandle {
    pub const ALL: [ResizeHandle; 8] = [
        ResizeHandle::TopLeft,
        ResizeHandle::Top,
        ResizeHandle::TopRight,
        ResizeHandle::Right,
        ResizeHandle::BottomRight,
        ResizeHandle::Bottom,
        ResizeHandle::BottomLeft,
        ResizeHandle::Left,
    ];

    /// Whether this handle influences the horizontal edges.
    pub const fn is_horizontal(self) -> bool {
        matches!(
            self,
            ResizeHandle::TopLeft
                | ResizeHandle::Top
                | ResizeHandle::TopRight
                | ResizeHandle::BottomRight
                | ResizeHandle::Bottom
                | ResizeHandle::BottomLeft
        )
    }
}

/// What interaction the selection is currently in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionInteraction {
    Idle,
    /// Pointer hovering (no button pressed).
    Hovering,
    /// Creating a free-drag selection.
    Dragging,
    /// Moving an existing selection.
    Moving,
    /// Resizing via one of the handles.
    Resizing(ResizeHandle),
}

/// The state of the selection while in `Selecting`.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionSession {
    pub rect: PhysicalRect,
    pub interaction: SelectionInteraction,
    pub drag_origin: Option<PhysicalPoint>,
    /// Optional bounds (e.g. the captured frame) that moves/resizes are kept
    /// inside. When `None`, selection is unrestricted.
    pub clamp_bounds: Option<PhysicalRect>,
    /// Minimum selection size enforced on commit and resize.
    pub min_size: u32,
}

impl Default for SelectionSession {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionSession {
    pub fn new() -> Self {
        Self {
            rect: PhysicalRect::default(),
            interaction: SelectionInteraction::Idle,
            drag_origin: None,
            clamp_bounds: None,
            min_size: 1,
        }
    }

    pub fn with_clamp_bounds(mut self, bounds: PhysicalRect) -> Self {
        self.clamp_bounds = Some(bounds);
        self
    }

    pub fn set_clamp_bounds(&mut self, bounds: Option<PhysicalRect>) {
        self.clamp_bounds = bounds;
    }

    pub fn begin_free_selection(&mut self, point: PhysicalPoint) {
        self.drag_origin = Some(point);
        self.rect = PhysicalRect::from_points(point, point);
        self.interaction = SelectionInteraction::Dragging;
    }

    pub fn update_free_selection(&mut self, point: PhysicalPoint) {
        if let Some(origin) = self.drag_origin {
            self.rect = clamp_rect_to_bounds(
                PhysicalRect::from_points(origin, point),
                self.clamp_bounds,
            );
        }
    }

    /// Finalize the current free selection. Returns the normalized selection
    /// rect (empty when there was no drag).
    pub fn commit_selection(&mut self) -> PhysicalRect {
        self.interaction = SelectionInteraction::Idle;
        self.drag_origin = None;
        self.rect
    }

    pub fn begin_move(&mut self) {
        self.interaction = SelectionInteraction::Moving;
    }

    pub fn move_by(&mut self, delta: PhysicalPoint) {
        self.rect = clamp_rect_to_bounds(self.rect.translate(delta), self.clamp_bounds);
    }

    pub fn begin_resize(&mut self, handle: ResizeHandle) {
        self.interaction = SelectionInteraction::Resizing(handle);
    }

    pub fn resize_to(&mut self, handle: ResizeHandle, target: PhysicalPoint) {
        self.rect = resize_rect(self.rect, handle, target, self.min_size, self.clamp_bounds);
    }

    pub fn set_hovering(&mut self) {
        self.interaction = SelectionInteraction::Hovering;
    }

    pub fn set_idle(&mut self) {
        self.interaction = SelectionInteraction::Idle;
    }

    pub fn is_active(&self) -> bool {
        !self.rect.is_empty()
    }

    /// Does `p` hit a resize handle (within a small tolerance)? Returns the hit
    /// handle if any.
    pub fn hit_resize_handle(&self, p: PhysicalPoint, tolerance: u32) -> Option<ResizeHandle> {
        let t = tolerance as i32;
        for &handle in &ResizeHandle::ALL {
            let corner = handle_corner(self.rect, handle);
            if (corner.x - p.x).abs() <= t && (corner.y - p.y).abs() <= t {
                return Some(handle);
            }
        }
        None
    }
}

fn handle_corner(rect: PhysicalRect, handle: ResizeHandle) -> PhysicalPoint {
    let left = rect.origin.x;
    let top = rect.origin.y;
    let right = rect.right();
    let bottom = rect.bottom();
    match handle {
        ResizeHandle::TopLeft => PhysicalPoint::new(left, top),
        ResizeHandle::Top => PhysicalPoint::new((left + right) / 2, top),
        ResizeHandle::TopRight => PhysicalPoint::new(right, top),
        ResizeHandle::Right => PhysicalPoint::new(right, (top + bottom) / 2),
        ResizeHandle::BottomRight => PhysicalPoint::new(right, bottom),
        ResizeHandle::Bottom => PhysicalPoint::new((left + right) / 2, bottom),
        ResizeHandle::BottomLeft => PhysicalPoint::new(left, bottom),
        ResizeHandle::Left => PhysicalPoint::new(left, (top + bottom) / 2),
    }
}

/// Resize `rect` by moving one edge to `target`, keeping the opposite edges
/// fixed. Enforces `min_size` and clamps to `clamp_bounds`.
pub fn resize_rect(
    rect: PhysicalRect,
    handle: ResizeHandle,
    target: PhysicalPoint,
    min_size: u32,
    clamp_bounds: Option<PhysicalRect>,
) -> PhysicalRect {
    let min = min_size.max(1) as i32;
    let left = rect.origin.x;
    let top = rect.origin.y;
    let right = rect.right();
    let bottom = rect.bottom();

    let (mut nl, mut nt, mut nr, mut nb) = (left, top, right, bottom);
    match handle {
        ResizeHandle::Left => nl = target.x.min(nr - min),
        ResizeHandle::Right => nr = target.x.max(nl + min),
        ResizeHandle::Top => nt = target.y.min(nb - min),
        ResizeHandle::Bottom => nb = target.y.max(nt + min),
        ResizeHandle::TopLeft => {
            nl = target.x.min(nr - min);
            nt = target.y.min(nb - min);
        }
        ResizeHandle::TopRight => {
            nr = target.x.max(nl + min);
            nt = target.y.min(nb - min);
        }
        ResizeHandle::BottomLeft => {
            nl = target.x.min(nr - min);
            nb = target.y.max(nt + min);
        }
        ResizeHandle::BottomRight => {
            nr = target.x.max(nl + min);
            nb = target.y.max(nt + min);
        }
    }

    let resized = PhysicalRect::from_points(
        PhysicalPoint::new(nl, nt),
        PhysicalPoint::new(nr, nb),
    );
    clamp_rect_to_bounds(resized, clamp_bounds)
}

fn clamp_rect_to_bounds(rect: PhysicalRect, bounds: Option<PhysicalRect>) -> PhysicalRect {
    match bounds {
        Some(b) => rect.clamp(b),
        None => rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> PhysicalPoint {
        PhysicalPoint::new(x, y)
    }
    fn r(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
        PhysicalRect::new(p(x, y), crate::geometry::PhysicalSize::new(w, h))
    }

    #[test]
    fn free_drag_normalizes() {
        let mut s = SelectionSession::new();
        s.set_clamp_bounds(Some(r(0, 0, 200, 200)));
        s.begin_free_selection(p(50, 80));
        s.update_free_selection(p(150, 30));
        assert_eq!(s.rect, r(50, 30, 100, 50));
        assert_eq!(s.commit_selection(), r(50, 30, 100, 50));
    }

    #[test]
    fn move_stays_inside_clamp_bounds() {
        let mut s = SelectionSession::new();
        s.set_clamp_bounds(Some(r(0, 0, 100, 100)));
        s.rect = r(40, 40, 20, 20);
        s.move_by(p(100, 100));
        assert_eq!(s.rect, r(80, 80, 20, 20));
    }

    #[test]
    fn resize_by_handle_respects_min_size() {
        let rect = r(10, 10, 100, 100);
        // Drag the left edge far past the right edge; it must clamp to min size.
        let out = resize_rect(rect, ResizeHandle::Left, p(108, 70), 5, None);
        assert_eq!(out.origin.x, rect.right() - 5);
    }

    #[test]
    fn resize_bottom_right_expands() {
        let rect = r(0, 0, 10, 10);
        let out = resize_rect(rect, ResizeHandle::BottomRight, p(50, 50), 1, None);
        assert_eq!(out, r(0, 0, 50, 50));
    }

    #[test]
    fn corner_handles_move_both_edges() {
        let rect = r(0, 0, 100, 100);
        let out = resize_rect(rect, ResizeHandle::TopLeft, p(20, 30), 1, None);
        assert_eq!(out, r(20, 30, 80, 70));
    }

    #[test]
    fn hit_resize_handle_returns_nearby_corner() {
        let s = SelectionSession {
            rect: r(0, 0, 100, 100),
            ..Default::default()
        };
        assert_eq!(s.hit_resize_handle(p(2, 0), 5), Some(ResizeHandle::TopLeft));
        assert_eq!(s.hit_resize_handle(p(100, 100), 5), Some(ResizeHandle::BottomRight));
        assert_eq!(s.hit_resize_handle(p(50, 50), 5), None);
    }
}
