//! `capture-render` — flattens a capture document (frame + annotations) into a
//! single RGBA8 bitmap and encodes PNG. This is the shared final-image pipeline
//! both frontends use for Copy / Save / Pin / AskAI.

use capture_annotation::{Annotation, CaptureDocument, Color};
use capture_core::geometry::PhysicalPoint;
use std::io::Write;

/// A flat RGBA8 bitmap.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes, top-left origin.
    pub pixels: Vec<u8>,
}

impl RenderedImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }
}

/// Error type for the renderer.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("document invalid: {0}")]
    InvalidDocument(String),
    #[error("png encode failed: {0}")]
    PngEncode(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Flatten a document into a final RGBA8 bitmap (crop + annotations composited).
pub fn flatten(document: &CaptureDocument) -> Result<RenderedImage, RenderError> {
    if let Err(e) = document.validate() {
        return Err(RenderError::InvalidDocument(e));
    }
    let source = &document.source;
    let crop = document.crop;
    let cw = crop.size.width as usize;
    let ch = crop.size.height as usize;
    let bpp = capture_core::PixelFormat::Rgba8.bytes_per_pixel() as usize;
    let stride = source.stride as usize;
    let src_origin = source.origin;
    let src_w = source.width as i64;
    let src_h = source.height as i64;

    let mut pixels = vec![0u8; cw * ch * bpp];

    // 1. Copy the crop region from the frame into the output (top-left origin).
    for y in 0..ch {
        let v_y = crop.origin.y as i64 + y as i64;
        let local_y = v_y - src_origin.y as i64;
        if local_y < 0 || local_y >= src_h {
            continue;
        }
        for x in 0..cw {
            let v_x = crop.origin.x as i64 + x as i64;
            let local_x = v_x - src_origin.x as i64;
            if local_x < 0 || local_x >= src_w {
                continue;
            }
            let src_idx = (local_y as usize) * stride + (local_x as usize) * bpp;
            let dst_idx = (y * cw + x) * bpp;
            let src = source.pixels.get(src_idx..src_idx + bpp).unwrap_or(&[0; 4]);
            pixels[dst_idx..dst_idx + bpp].copy_from_slice(&src[..4]);
        }
    }

    // 2. Composite annotations in frame-absolute coordinates, offset by the crop.
    let mut canvas = Canvas {
        pixels: &mut pixels,
        width: cw as u32,
        height: ch as u32,
        offset_x: -crop.origin.x,
        offset_y: -crop.origin.y,
    };
    for ann in &document.annotations {
        canvas.draw_annotation(ann);
    }

    Ok(RenderedImage::new(cw as u32, ch as u32, pixels))
}

/// Encode a rendered image as PNG bytes.
pub fn encode_png(image: &RenderedImage) -> Result<Vec<u8>, RenderError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| RenderError::PngEncode(e.to_string()))?;
        writer
            .write_image_data(&image.pixels)
            .map_err(|e| RenderError::PngEncode(e.to_string()))?;
    }
    Ok(out)
}

/// Encode and write PNG to a path.
pub fn save_png(path: impl AsRef<std::path::Path>, image: &RenderedImage) -> Result<(), RenderError> {
    let bytes = encode_png(image)?;
    let mut file = std::fs::File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

/// A simple, fast, deterministic checksum used by golden tests (FNV-1a).
pub fn checksum(image: &RenderedImage) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in &image.pixels {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Canvas rasterisation helpers.
// ---------------------------------------------------------------------------

struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: u32,
    height: u32,
    /// Offset added to a frame-absolute point to get a crop-local pixel coord.
    offset_x: i32,
    offset_y: i32,
}

impl<'a> Canvas<'a> {
    fn in_bounds(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    /// Blend a color over the existing pixel using simple (non-premultiplied)
    /// alpha compositing.
    fn blend(&mut self, x: i32, y: i32, color: Color) {
        if !self.in_bounds(x, y) {
            return;
        }
        let idx = ((y as u32 * self.width) + x as u32) as usize * 4;
        let a = color.a as u32;
        if a == 0 {
            return;
        }
        if a == 255 {
            self.pixels[idx] = color.r;
            self.pixels[idx + 1] = color.g;
            self.pixels[idx + 2] = color.b;
            self.pixels[idx + 3] = 255;
            return;
        }
        let inv = 255 - a;
        for (i, &src) in [color.r, color.g, color.b].iter().enumerate() {
            let dst = self.pixels[idx + i] as u32;
            let blended = (src as u32 * a + dst * inv) / 255;
            self.pixels[idx + i] = blended as u8;
        }
        let dst_a = self.pixels[idx + 3] as u32;
        let out_a = a + (dst_a * inv) / 255;
        self.pixels[idx + 3] = out_a.min(255) as u8;
    }

    fn stamp_disc(&mut self, cx: i32, cy: i32, radius: i32, color: Color) {
        if radius <= 0 {
            self.blend(cx, cy, color);
            return;
        }
        let r2 = radius * radius;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= r2 {
                    self.blend(cx + dx, cy + dy, color);
                }
            }
        }
    }

    fn thick_line(&mut self, p0: PhysicalPoint, p1: PhysicalPoint, thickness: u32, color: Color) {
        let radius = (thickness as i32) / 2;
        let (mut x0, mut y0) = (p0.x, p0.y);
        let (x1, y1) = (p1.x, p1.y);
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;
        loop {
            self.stamp_disc(x0, y0, radius, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 > -dy {
                err -= dy;
                x0 += sx;
            }
            if e2 < dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    fn draw_annotation(&mut self, annotation: &Annotation) {
        match annotation {
            Annotation::Pen(stroke) => {
                let pts: Vec<_> = stroke
                    .points
                    .iter()
                    .map(|p| {
                        PhysicalPoint::new(p.x + self.offset_x, p.y + self.offset_y)
                    })
                    .collect();
                if pts.len() == 1 {
                    self.stamp_disc(pts[0].x, pts[0].y, (stroke.thickness as i32) / 2, stroke.color);
                    return;
                }
                for w in pts.windows(2) {
                    self.thick_line(w[0], w[1], stroke.thickness, stroke.color);
                }
            }
            Annotation::Rectangle(shape) => {
                let rect = shape
                    .rect
                    .translate(PhysicalPoint::new(self.offset_x, self.offset_y));
                // Fill (if any) then outline.
                if let Some(fill) = shape.fill {
                    for y in rect.top()..rect.bottom() {
                        for x in rect.left()..rect.right() {
                            self.blend(x, y, fill);
                        }
                    }
                }
                let t = shape.thickness;
                // Four edges as thick lines.
                let tl = rect.origin;
                let tr = PhysicalPoint::new(rect.right(), rect.top());
                let br = PhysicalPoint::new(rect.right(), rect.bottom());
                let bl = PhysicalPoint::new(rect.left(), rect.bottom());
                self.thick_line(tl, tr, t, shape.color);
                self.thick_line(tr, br, t, shape.color);
                self.thick_line(br, bl, t, shape.color);
                self.thick_line(bl, tl, t, shape.color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_annotation::document::{PenStroke, RectShape};
    use capture_core::capture::{CapturedFrame, PixelFormat};
    use capture_core::geometry::{PhysicalRect, PhysicalSize};
    use std::sync::Arc;

    fn frame(w: u32, h: u32, fill: u8) -> CapturedFrame {
        let bytes = (w * h * 4) as usize;
        CapturedFrame::new(
            vec![fill; bytes].into(),
            w,
            h,
            w * 4,
            PhysicalPoint::new(0, 0),
            PixelFormat::Rgba8,
        )
    }

    #[test]
    fn flatten_crop_only_matches_source() {
        // 20x10 frame filled with value 0x11; crop the 10x5 top-left region.
        let mut doc = CaptureDocument::new(Arc::new(frame(20, 10, 0x11)), PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(10, 5)));
        let img = flatten(&doc).unwrap();
        assert_eq!(img.width, 10);
        assert_eq!(img.height, 5);
        assert!(img.pixels.iter().all(|&b| b == 0x11));
        doc.annotations.push(Annotation::Rectangle(RectShape::new(
            PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(2, 2)),
            Color::RED,
            1,
            None,
        )));
        let img2 = flatten(&doc).unwrap();
        assert_ne!(img.pixels, img2.pixels);
    }

    #[test]
    fn crop_offset_from_negative_origin_frame() {
        // Frame whose origin is negative (monitor to the left of primary).
        let mut frame = frame(100, 100, 0x22);
        frame.origin = PhysicalPoint::new(-1000, 0);
        let crop = PhysicalRect::new(PhysicalPoint::new(-1000, 0), PhysicalSize::new(40, 40));
        let doc = CaptureDocument::new(Arc::new(frame), crop);
        let img = flatten(&doc).unwrap();
        assert!(img.pixels.iter().all(|&b| b == 0x22));
    }

    #[test]
    fn render_pen_stroke_produces_deterministic_output() {
        let mut doc = CaptureDocument::new(Arc::new(frame(64, 64, 0x00)), PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(64, 64)));
        doc.annotations.push(Annotation::Pen(PenStroke {
            color: Color::WHITE,
            thickness: 5,
            points: vec![
                PhysicalPoint::new(10, 10),
                PhysicalPoint::new(30, 10),
            ],
        }));
        let a = flatten(&doc).unwrap();
        let b = flatten(&doc).unwrap();
        assert_eq!(a.pixels, b.pixels);
        assert!(a.pixels.iter().any(|&x| x != 0)); // something drawn
    }

    /// Golden test: a fixed document must always flatten to a fixed checksum.
    #[test]
    fn golden_checksum_is_stable() {
        let mut doc = CaptureDocument::new(Arc::new(frame(96, 64, 0x3C)), PhysicalRect::new(PhysicalPoint::new(8, 8), PhysicalSize::new(80, 48)));
        doc.annotations.push(Annotation::Rectangle(RectShape::new(
            PhysicalRect::new(PhysicalPoint::new(12, 12), PhysicalSize::new(40, 24)),
            Color::BLUE,
            3,
            Some(Color::new(255, 0, 0, 128)),
        )));
        doc.annotations.push(Annotation::Pen(PenStroke {
            color: Color::YELLOW,
            thickness: 4,
            points: vec![
                PhysicalPoint::new(20, 20),
                PhysicalPoint::new(60, 20),
                PhysicalPoint::new(60, 50),
            ],
        }));
        let img = flatten(&doc).unwrap();
        // This value is the "golden" reference. Updating it intentionally is
        // the only way to change the renderer output of this fixture.
        assert_eq!(checksum(&img), 0x40DC0DDD09DFFF4B);
    }

    #[test]
    fn png_round_trip_includes_header() {
        let doc = CaptureDocument::new(Arc::new(frame(16, 16, 0x12)), PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(16, 16)));
        let img = flatten(&doc).unwrap();
        let png = encode_png(&img).unwrap();
        assert!(png.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
    }
}
