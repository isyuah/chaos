//! Core geometry primitives.
//!
//! The canonical coordinate space is **physical pixels**. All `Physical*` types
//! are integer-based and may be negative where a monitor sits to the left of or
//! above the primary monitor. `Logical*` types exist only at the UI bridge.

/// A point in physical-pixel coordinates relative to the global virtual desktop
/// origin. Coordinates may be negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

impl PhysicalPoint {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub const ZERO: Self = Self { x: 0, y: 0 };

    /// Component-wise add.
    pub const fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
        }
    }

    /// Component-wise subtract.
    pub const fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
        }
    }
}

impl std::ops::Add for PhysicalPoint {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.add(rhs)
    }
}

impl std::ops::Sub for PhysicalPoint {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.sub(rhs)
    }
}

/// A size in physical pixels. Width/height are non-negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}

impl PhysicalSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub const ZERO: Self = Self {
        width: 0,
        height: 0,
    };

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn area(self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }
}

/// A rectangle in physical-pixel coordinates. Stored as origin + size.
///
/// A freshly constructed rect is always normalized so `origin` is the top-left
/// and `size` is non-negative (use [`PhysicalRect::from_points`] for drags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PhysicalRect {
    pub origin: PhysicalPoint,
    pub size: PhysicalSize,
}

impl PhysicalRect {
    pub const fn new(origin: PhysicalPoint, size: PhysicalSize) -> Self {
        Self { origin, size }
    }

    /// Build a rect from two points (e.g. a drag start/end). Normalizes the
    /// order so the result always has a top-left origin and positive size.
    pub fn from_points(a: PhysicalPoint, b: PhysicalPoint) -> Self {
        let left = a.x.min(b.x);
        let top = a.y.min(b.y);
        let right = a.x.max(b.x);
        let bottom = a.y.max(b.y);
        Self {
            origin: PhysicalPoint::new(left, top),
            size: PhysicalSize::new(
                (right - left).max(0) as u32,
                (bottom - top).max(0) as u32,
            ),
        }
    }

    pub const fn right(self) -> i32 {
        self.origin.x + self.size.width as i32
    }

    pub const fn bottom(self) -> i32 {
        self.origin.y + self.size.height as i32
    }

    pub const fn left(self) -> i32 {
        self.origin.x
    }

    pub const fn top(self) -> i32 {
        self.origin.y
    }

    pub const fn width(self) -> u32 {
        self.size.width
    }

    pub const fn height(self) -> u32 {
        self.size.height
    }

    pub const fn is_empty(self) -> bool {
        self.size.is_empty()
    }

    pub const fn area(self) -> u64 {
        self.size.area()
    }

    pub const fn center(self) -> PhysicalPoint {
        PhysicalPoint::new(
            self.origin.x + (self.size.width as i32) / 2,
            self.origin.y + (self.size.height as i32) / 2,
        )
    }

    /// Does the rect contain `p` (inclusive of the bottom-right edge)?
    pub fn contains(self, p: PhysicalPoint) -> bool {
        p.x >= self.origin.x && p.x <= self.right() && p.y >= self.origin.y && p.y <= self.bottom()
    }

    /// Point containment with an exclusive bottom-right edge (standard for a
    /// half-open canvas region).
    pub fn contains_exclusive(self, p: PhysicalPoint) -> bool {
        p.x >= self.origin.x && p.x < self.right() && p.y >= self.origin.y && p.y < self.bottom()
    }

    /// Tests whether two rects overlap (touching edges count as no overlap).
    pub fn intersects(self, other: Self) -> bool {
        self.intersection(other).is_some()
    }

    /// Axis-aligned intersection. Returns `None` when the rects do not overlap.
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.origin.x.max(other.origin.x);
        let top = self.origin.y.max(other.origin.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= left || bottom <= top {
            return None;
        }
        Some(Self::from_points(
            PhysicalPoint::new(left, top),
            PhysicalPoint::new(right, bottom),
        ))
    }

    /// Translate the rect by `delta`.
    pub const fn translate(self, delta: PhysicalPoint) -> Self {
        Self {
            origin: self.origin.add(delta),
            size: self.size,
        }
    }

    /// Clamp the rect so it lies fully inside `bounds`. The origin is clamped
    /// into the range that keeps the rect's size; if the rect is larger than
    /// `bounds` in an axis, its size shrinks to fit that axis.
    pub fn clamp(self, bounds: Self) -> Self {
        let w = self.size.width as i32;
        let h = self.size.height as i32;
        let bw = bounds.size.width as i32;
        let bh = bounds.size.height as i32;

        let (x, out_w) = if w >= bw {
            (bounds.origin.x, bounds.size.width)
        } else {
            let max_x = bounds.right() - w;
            (self.origin.x.clamp(bounds.origin.x, max_x), self.size.width)
        };
        let (y, out_h) = if h >= bh {
            (bounds.origin.y, bounds.size.height)
        } else {
            let max_y = bounds.bottom() - h;
            (self.origin.y.clamp(bounds.origin.y, max_y), self.size.height)
        };

        Self::new(PhysicalPoint::new(x, y), PhysicalSize::new(out_w, out_h))
    }

    /// Grow (`amount > 0`) or shrink (`amount < 0`) the rect on each side.
    /// A deflation that would invert the rect collapses it to an empty rect at
    /// the clamped corner.
    pub fn inflate(self, amount: i32) -> Self {
        let left = self.origin.x - amount;
        let top = self.origin.y - amount;
        let right = self.right() + amount;
        let bottom = self.bottom() + amount;
        if left <= right && top <= bottom {
            Self::from_points(PhysicalPoint::new(left, top), PhysicalPoint::new(right, bottom))
        } else {
            Self::new(
                PhysicalPoint::new(left.min(right), top.min(bottom)),
                PhysicalSize::new(0, 0),
            )
        }
    }

    /// The union (bounding box) of two rects.
    pub fn union(self, other: Self) -> Self {
        Self::from_points(
            PhysicalPoint::new(
                self.origin.x.min(other.origin.x),
                self.origin.y.min(other.origin.y),
            ),
            PhysicalPoint::new(
                self.right().max(other.right()),
                self.bottom().max(other.bottom()),
            ),
        )
    }

    /// The inset rectangle by the given amounts (values must be >= 0).
    pub fn inset(self, left: u32, top: u32, right: u32, bottom: u32) -> Self {
        let origin = PhysicalPoint::new(self.origin.x + left as i32, self.origin.y + top as i32);
        let width = (self.width() - left - right).max(0);
        let height = (self.height() - top - bottom).max(0);
        Self::new(origin, PhysicalSize::new(width, height))
    }
}

/// A per-monitor scale factor: physical pixels per logical pixel.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScaleFactor(pub f64);

impl ScaleFactor {
    pub const fn new(value: f64) -> Self {
        Self(value)
    }

    pub const fn from_f32(value: f32) -> Self {
        Self(value as f64)
    }

    pub const fn get(self) -> f64 {
        self.0
    }

    /// A sensible fallback when the OS reports 0 or NaN.
    pub fn sanitized(self) -> f64 {
        if self.0.is_finite() && self.0 > 0.0 {
            self.0
        } else {
            1.0
        }
    }
}

// ---------------------------------------------------------------------------
// Logical (toolkit) types. These exist only at the UI bridge.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalRect {
    pub origin: LogicalPoint,
    pub size: LogicalSize,
}

impl LogicalPoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl LogicalSize {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

impl LogicalRect {
    pub const fn new(origin: LogicalPoint, size: LogicalSize) -> Self {
        Self { origin, size }
    }

    pub const fn right(self) -> f32 {
        self.origin.x + self.size.width
    }

    pub const fn bottom(self) -> f32 {
        self.origin.y + self.size.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> PhysicalPoint {
        PhysicalPoint::new(x, y)
    }

    fn s(w: u32, h: u32) -> PhysicalSize {
        PhysicalSize::new(w, h)
    }

    fn r(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
        PhysicalRect::new(p(x, y), s(w, h))
    }

    #[test]
    fn rect_from_points_normalizes_negative_drag() {
        // Drag from bottom-right to top-left.
        let rect = PhysicalRect::from_points(p(10, 20), p(-5, 4));
        assert_eq!(rect.origin, p(-5, 4));
        assert_eq!(rect.size, s(15, 16));
    }

    #[test]
    fn negative_virtual_coordinates_are_supported() {
        // A monitor whose origin is negative (secondary to the left).
        let monitor = r(-1920, 0, 1920, 1080);
        assert_eq!(monitor.origin.x, -1920);
        assert!(monitor.contains(p(-100, 500)));
        assert!(monitor.contains(p(-1920, 0)));
        assert!(monitor.contains(p(-1, 1079)));
        // The right edge (x = -1920 + 1920 = 0) is exclusive for a pixel region.
        assert!(!monitor.contains_exclusive(p(0, 0)));
        assert!(!monitor.contains(p(1, 0)));
        assert_eq!(monitor.right(), 0);
        assert_eq!(monitor.center(), p(-960, 540));
    }

    #[test]
    fn intersection_overlap() {
        assert_eq!(r(0, 0, 10, 10).intersection(r(5, 5, 10, 10)), Some(r(5, 5, 5, 5)));
        assert_eq!(r(0, 0, 10, 10).intersection(r(10, 10, 10, 10)), None); // touching edges
        assert_eq!(r(-5, -5, 3, 3).intersection(r(0, 0, 10, 10)), None);
    }

    #[test]
    fn clamp_stays_inside_bounds() {
        let out = r(100, 100, 50, 50).clamp(r(0, 0, 100, 100));
        // origin clamped so the rect fits.
        assert_eq!(out.origin, p(50, 50));
        assert_eq!(out.size, s(50, 50));
    }

    #[test]
    fn clamp_keeps_full_rect_when_bounds_enough() {
        let out = r(20, 20, 30, 30).clamp(r(0, 0, 100, 100));
        assert_eq!(out, r(20, 20, 30, 30));
    }

    #[test]
    fn clamp_negative_origin_bounds() {
        // bounds extends into negative space.
        let out = r(-3000, 500, 100, 100).clamp(r(-1920, 0, 1920, 1080));
        assert_eq!(out.origin, p(-1920, 500));
        assert_eq!(out.size, s(100, 100));
    }

    #[test]
    fn clamp_larger_than_bounds_shrinks() {
        let out = r(0, 0, 500, 500).clamp(r(0, 0, 100, 100));
        assert_eq!(out, r(0, 0, 100, 100));
    }

    #[test]
    fn translate_moves_origin() {
        assert_eq!(r(10, 10, 5, 5).translate(p(-2, 3)), r(8, 13, 5, 5));
    }

    #[test]
    fn inflate_grows_and_stays_positive() {
        assert_eq!(r(10, 10, 5, 5).inflate(2), r(8, 8, 9, 9));
        // A huge deflation collapses to empty rather than inverting.
        assert!(r(10, 10, 2, 2).inflate(-10).is_empty());
    }

    #[test]
    fn union_bounding_box() {
        assert_eq!(r(0, 0, 10, 10).union(r(-5, 2, 4, 4)), r(-5, 0, 15, 10));
    }

    #[test]
    fn contains_exclusive_edges() {
        let r = r(0, 0, 10, 10);
        assert!(r.contains_exclusive(p(9, 9)));
        assert!(!r.contains_exclusive(p(10, 10)));
    }

    #[test]
    fn scale_factor_sanitize() {
        assert_eq!(ScaleFactor::new(1.25).sanitized(), 1.25);
        assert_eq!(ScaleFactor::new(0.0).sanitized(), 1.0);
        assert_eq!(ScaleFactor::new(f64::NAN).sanitized(), 1.0);
    }
}
