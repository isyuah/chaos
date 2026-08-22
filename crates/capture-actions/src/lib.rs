//! `capture-actions` — the shared, toolkit-independent actions (Copy / Save /
//! Pin / AskAI).
//!
//! Core only **produces payloads** (final PNG bytes + metadata) and, for Save,
//! writes a file. Writing to the OS clipboard or creating a Pin window is the
//! caller's (frontend shell's) job — Core never touches a UI event loop.

use capture_annotation::CaptureDocument;
use capture_core::ActionId;
use capture_render::{self, RenderError};
use std::path::PathBuf;

/// The payload any action hands to the caller.
#[derive(Debug, Clone)]
pub struct ActionPayload {
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl ActionPayload {
    pub fn png(&self) -> &[u8] {
        &self.png_bytes
    }
}

/// The result of invoking an action.
#[derive(Debug, Clone)]
pub enum ActionOutcome {
    /// A PNG image payload ready to copy to the clipboard or an external consumer.
    Png(ActionPayload),
    /// The path where `SaveAction` wrote the PNG.
    Saved(PathBuf),
    /// Payload for the Pin frontend window (the window itself is the frontend's).
    Pin(ActionPayload),
    /// Payload to hand to an AI consumer (stub; no real model call).
    AskAi(ActionPayload),
}

/// Error type produced by an action.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("render failed: {0}")]
    Render(#[from] RenderError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("action failed: {0}")]
    Action(String),
}

/// A user-facing capture action. UI-neutral and toolkit-free.
pub trait CaptureAction: Send + Sync {
    /// A stable, tool-independent id (e.g. "copy").
    fn id(&self) -> &'static str;

    fn action_id(&self) -> ActionId {
        ActionId::new(self.id())
    }

    fn invoke(&self, document: &CaptureDocument) -> Result<ActionOutcome, ActionError>;
}

/// Copy: produce the final PNG bytes for the clipboard.
#[derive(Debug, Default, Clone)]
pub struct CopyAction;

impl CaptureAction for CopyAction {
    fn id(&self) -> &'static str {
        "copy"
    }

    fn invoke(&self, document: &CaptureDocument) -> Result<ActionOutcome, ActionError> {
        Ok(ActionOutcome::Png(payload_from(document)?))
    }
}

/// Save: flatten, encode, and write a PNG to disk.
#[derive(Debug, Clone)]
pub struct SaveAction {
    pub path: PathBuf,
}

impl SaveAction {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
        }
    }
}

impl CaptureAction for SaveAction {
    fn id(&self) -> &'static str {
        "save"
    }

    fn invoke(&self, document: &CaptureDocument) -> Result<ActionOutcome, ActionError> {
        let image = capture_render::flatten(document)?;
        capture_render::save_png(&self.path, &image)?;
        Ok(ActionOutcome::Saved(self.path.clone()))
    }
}

/// Pin: produce the payload the frontend's Pin window displays.
#[derive(Debug, Default, Clone)]
pub struct PinAction;

impl CaptureAction for PinAction {
    fn id(&self) -> &'static str {
        "pin"
    }

    fn invoke(&self, document: &CaptureDocument) -> Result<ActionOutcome, ActionError> {
        Ok(ActionOutcome::Pin(payload_from(document)?))
    }
}

/// AskAI: produce a payload to hand to an external consumer. Stub — no real
/// model call.
#[derive(Debug, Default, Clone)]
pub struct AskAiAction;

impl CaptureAction for AskAiAction {
    fn id(&self) -> &'static str {
        "ask-ai"
    }

    fn invoke(&self, document: &CaptureDocument) -> Result<ActionOutcome, ActionError> {
        Ok(ActionOutcome::AskAi(payload_from(document)?))
    }
}

fn payload_from(document: &CaptureDocument) -> Result<ActionPayload, ActionError> {
    let image = capture_render::flatten(document)?;
    let png_bytes = capture_render::encode_png(&image)?;
    Ok(ActionPayload {
        png_bytes,
        width: image.width,
        height: image.height,
    })
}

/// All demo actions, keyed by [`ActionId`]. Useful for building a toolbar.
pub fn all_actions() -> Vec<Box<dyn CaptureAction>> {
    vec![
        Box::new(CopyAction),
        Box::new(PinAction),
        Box::new(AskAiAction),
    ]
}

/// Look up an action by its id.
pub fn action_by_id(id: ActionId) -> Option<Box<dyn CaptureAction>> {
    match id.as_str() {
        "copy" => Some(Box::new(CopyAction)),
        "pin" => Some(Box::new(PinAction)),
        "ask-ai" => Some(Box::new(AskAiAction)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_annotation::document::{Annotation, Color, PenStroke};
    use capture_core::geometry::{PhysicalPoint, PhysicalRect, PhysicalSize};
    use capture_core::capture::{CapturedFrame, PixelFormat};
    use std::sync::Arc;

    fn doc() -> CaptureDocument {
        let w = 40u32;
        let h = 30u32;
        let frame = CapturedFrame::new(
            vec![0x55u8; (w * h * 4) as usize].into(),
            w,
            h,
            w * 4,
            PhysicalPoint::new(0, 0),
            PixelFormat::Rgba8,
        );
        let mut doc = CaptureDocument::new(
            Arc::new(frame),
            PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(20, 20)),
        );
        doc.push_annotation(Annotation::Pen(PenStroke {
            color: Color::RED,
            thickness: 2,
            points: vec![
                PhysicalPoint::new(3, 3),
                PhysicalPoint::new(12, 10),
            ],
        }));
        doc
    }

    #[test]
    fn copy_produces_non_empty_png() {
        let outcome = CopyAction.invoke(&doc()).unwrap();
        match outcome {
            ActionOutcome::Png(p) => {
                assert!(p.png_bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
                assert_eq!(p.width, 20);
                assert_eq!(p.height, 20);
            }
            _ => panic!("expected Png"),
        }
    }

    #[test]
    fn save_writes_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("capture_action_test_{}.png", std::process::id()));
        let outcome = SaveAction::new(&path).invoke(&doc()).unwrap();
        assert!(matches!(outcome, ActionOutcome::Saved(_)));
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ids_are_stable() {
        assert_eq!(CopyAction.id(), "copy");
        assert_eq!(SaveAction::new("x").id(), "save");
        assert_eq!(PinAction.id(), "pin");
        assert_eq!(AskAiAction.id(), "ask-ai");
    }
}
