//! Annotation document model, shared by Core, renderer, and actions.

use capture_core::geometry::{PhysicalPoint, PhysicalRect};
use capture_core::{CaptureError, CapturedFrame};
use std::sync::Arc;

/// An RGBA color (8 bits per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::new(r, g, b, a)
    }

    pub const fn opaque(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    pub const RED: Color = Color::opaque(255, 0, 0);
    pub const GREEN: Color = Color::opaque(0, 255, 0);
    pub const BLUE: Color = Color::opaque(0, 0, 255);
    pub const YELLOW: Color = Color::opaque(255, 255, 0);
    pub const BLACK: Color = Color::opaque(0, 0, 0);
    pub const WHITE: Color = Color::opaque(255, 255, 255);
}

/// A freehand pen stroke, stored in frame-absolute physical coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PenStroke {
    pub color: Color,
    pub thickness: u32,
    pub points: Vec<PhysicalPoint>,
}

impl PenStroke {
    pub fn new(color: Color, thickness: u32) -> Self {
        Self {
            color,
            thickness: thickness.max(1),
            points: Vec::new(),
        }
    }

    pub fn bounds(&self) -> Option<PhysicalRect> {
        if self.points.is_empty() {
            return None;
        }
        let mut min = self.points[0];
        let mut max = self.points[0];
        for p in &self.points {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        let r = PhysicalRect::from_points(min, max);
        // Pad by half the stroke thickness so the stroke fits visually.
        Some(r.inflate(self.thickness as i32 / 2))
    }
}

/// An axis-aligned rectangle annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct RectShape {
    pub rect: PhysicalRect,
    pub color: Color,
    pub thickness: u32,
    pub fill: Option<Color>,
}

impl RectShape {
    pub fn new(rect: PhysicalRect, color: Color, thickness: u32, fill: Option<Color>) -> Self {
        Self {
            rect,
            color,
            thickness: thickness.max(1),
            fill,
        }
    }
}

/// A single annotation on the document.
#[derive(Debug, Clone, PartialEq)]
pub enum Annotation {
    Pen(PenStroke),
    Rectangle(RectShape),
}

impl Annotation {
    pub fn color(&self) -> Color {
        match self {
            Annotation::Pen(p) => p.color,
            Annotation::Rectangle(r) => r.color,
        }
    }

    /// Bounding box of the annotation in frame-absolute physical coordinates.
    pub fn bounds(&self) -> Option<PhysicalRect> {
        match self {
            Annotation::Pen(p) => p.bounds(),
            Annotation::Rectangle(r) => Some(r.rect),
        }
    }
}

/// The full editing document: a source frame, a crop (selection), and the list
/// of annotations applied to that crop (in frame-absolute coordinates).
#[derive(Debug, Clone)]
pub struct CaptureDocument {
    pub source: Arc<CapturedFrame>,
    pub crop: PhysicalRect,
    pub annotations: Vec<Annotation>,
}

impl CaptureDocument {
    pub fn new(source: Arc<CapturedFrame>, crop: PhysicalRect) -> Self {
        Self {
            source,
            crop,
            annotations: Vec::new(),
        }
    }

    pub fn source_frame(&self) -> &CapturedFrame {
        &self.source
    }

    pub fn push_annotation(&mut self, annotation: Annotation) {
        self.annotations.push(annotation);
    }

    /// True when the crop fully lies within the source frame.
    pub fn validate(&self) -> Result<(), CaptureError> {
        self.source.validate()?;
        if self.crop.is_empty() {
            return Err(CaptureError::InvalidSelection("crop is empty".to_string()));
        }
        let bounds = self.source.bounds();
        if self.crop.origin.x < bounds.origin.x
            || self.crop.origin.y < bounds.origin.y
            || self.crop.right() > bounds.right()
            || self.crop.bottom() > bounds.bottom()
        {
            return Err(CaptureError::InvalidSelection(
                "crop extends outside the captured frame".to_string(),
            ));
        }
        Ok(())
    }
}
