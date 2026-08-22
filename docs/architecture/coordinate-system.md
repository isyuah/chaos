# Coordinate System

## 1. Canonical space

The Core's canonical coordinate space is **physical pixels**, relative to a
single global virtual-desktop origin.

- `PhysicalRect` may have a **negative** `origin.x` / `origin.y`. This is
  required for multi-monitor arrangements where a secondary monitor sits to the
  left of, or above, the primary monitor.
- `PhysicalPoint` uses `i32`; `PhysicalSize` uses `u32` (width/height are
  non-negative).
- A `PhysicalRect` is stored as `origin: PhysicalPoint` + `size: PhysicalSize`
  and is always **finalized** by `normalized()` converting any potentially
  inverted (drag) rectangle into a canonical top-left + positive size.
- `ScaleFactor` is an `f64` describing physical pixels per logical pixel.

## 2. Types

```rust
pub struct PhysicalPoint  { pub x: i32, pub y: i32 }
pub struct LogicalPoint   { pub x: f32, pub y: f32 }   // toolkit space
pub struct PhysicalSize   { pub width: u32, pub height: u32 }
pub struct LogicalSize    { pub width: f32, pub height: f32 }
pub struct PhysicalRect   { pub origin: PhysicalPoint, pub size: PhysicalSize }
pub struct LogicalRect    { pub origin: LogicalPoint,  pub size: LogicalSize  }
pub struct ScaleFactor    (pub f64);
```

Logical types exist only at the UI bridge; they are never the source of truth.

## 3. Coordinate Mapper contract

For mixed-DPI we need a per-monitor mapping between physical and logical
coordinates. One `CoordinateMapper` is defined per monitor.

```rust
pub struct CoordinateMapper {
    pub scale_factor: ScaleFactor,
    pub physical_origin: PhysicalPoint, // this monitor's top-left in virtual coords
}

impl CoordinateMapper {
    pub fn physical_to_logical(&self, p: PhysicalPoint) -> LogicalPoint;
    pub fn logical_to_physical(&self, p: LogicalPoint) -> PhysicalPoint;
    pub fn physical_rect_to_logical(&self, r: PhysicalRect) -> LogicalRect;
    pub fn logical_rect_to_physical(&self, r: LogicalRect) -> PhysicalRect;
}
```

Mappings are pure arithmetic on the single `scale_factor` of the monitor the
pointer is currently over; they do not attempt any cross-monitor scale
interpolation. Because coordinates are integer physical pixels on input and the
frame is captured in physical pixels, there is no rounding drift in the Core;
rounding only happens once, when converting a physical selection back to the
toolkit's float logical space for display.

## 4. Capture source alignment

A `CapturedFrame` is captured from the whole virtual desktop and then cropped to
the requested monitor. Its `origin` is the monitor's virtual top-left
(`PhysicalRect` negative-origin aware), and its pixels are the monitor's
physical-pixel content. Therefore:

```
frame.origin == monitor.bounds.origin
frame.width  == monitor.bounds.size.width
frame.height == monitor.bounds.size.height
```

## 5. DPI-awareness requirement

For `GetDC(NULL)`/`GetDIBits` to return physical pixels and for
`EnumDisplayMonitors` to report physical monitor rects, the process must be
Per-Monitor-V2 DPI aware. `capture-windows` calls
`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` at
platform construction. This is recorded in the DPI test matrix in
`CORE_BASELINE_REPORT.md`.

## 6. Toolbar placement is purely physical

`place_toolbar` receives a physical selection, a physical toolbar size, and a
physical work area, and returns a physical placement. It never consults logical
coordinates, so the two frontends get bit-identical placement results.
