//! `capture-annotation` — the annotation document, undo, and the capture session
//! state machine. Depends only on `capture-core` (see ADR-0003).

pub mod document;
pub mod session;
pub mod tools;

pub use document::{Annotation, CaptureDocument, Color, PenStroke, RectShape};
pub use session::{
    CaptureCommand, CaptureEvent, CaptureSession, CaptureSessionState, EditorSession,
};
pub use tools::AnnotationTool;
