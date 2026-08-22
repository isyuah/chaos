//! The centralized capture-session state machine.
//!
//! The frontend is the driver: it feeds [`CaptureCommand`]s (translated from OS
//! pointer/keyboard events + capture results) and consumes the [`CaptureEvent`]s
//! produced. The Core owns the state and the annotation document.

use crate::document::{Annotation, CaptureDocument, Color, PenStroke, RectShape};
use crate::tools::AnnotationTool;
use capture_core::action::ActionId;
use capture_core::capture::{CaptureError, CapturedFrame, Timing};
use capture_core::geometry::{PhysicalPoint, PhysicalRect};
use capture_core::selection::{resize_rect, ResizeHandle, SelectionInteraction, SelectionSession};
use capture_core::snap::SnapCandidate;
use std::sync::Arc;

/// The state of a capture session. UI-neutral and toolkit-free.
#[derive(Debug, Clone)]
pub enum CaptureSessionState {
    Idle,
    Preparing,
    Selecting(SelectionSession),
    Editing(EditorSession),
}

/// Input commands to the session.
#[derive(Debug, Clone)]
pub enum CaptureCommand {
    Begin,
    FrameReady(CapturedFrame),
    PointerMoved(PhysicalPoint),
    /// Frontend-driven hover candidate (from `SnapBackend`).
    SnapCandidate(Option<SnapCandidate>),
    BeginFreeSelection(PhysicalPoint),
    UpdateFreeSelection(PhysicalPoint),
    CommitSelection,
    MoveSelection(PhysicalPoint),
    ResizeSelection(ResizeHandle, PhysicalPoint),
    SelectTool(AnnotationTool),
    BeginAnnotation(PhysicalPoint),
    UpdateAnnotation(PhysicalPoint),
    EndAnnotation,
    Undo,
    Redo,
    InvokeAction(ActionId),
    Cancel,
}

/// Output events produced by [`CaptureSession::apply`].
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    StateChanged,
    SelectionChanged(PhysicalRect),
    SnapCandidateChanged(Option<SnapCandidate>),
    DocumentChanged,
    ToolChanged(AnnotationTool),
    ActionRequested(ActionId),
    Completed,
    Error(CaptureError),
}

const DEFAULT_PEN_THICKNESS: u32 = 3;
const DEFAULT_PEN_COLOR: Color = Color::RED;

/// The editing half of the state: the document plus undo/redo and the
/// in-progress annotation preview.
#[derive(Debug, Clone)]
pub struct EditorSession {
    pub document: CaptureDocument,
    pub selected_tool: AnnotationTool,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
    active: Option<ActiveAnnotation>,
}

#[derive(Debug, Clone)]
struct EditorSnapshot {
    crop: PhysicalRect,
    annotations: Vec<Annotation>,
}

#[derive(Debug, Clone)]
enum ActiveAnnotation {
    Pen {
        points: Vec<PhysicalPoint>,
        color: Color,
        thickness: u32,
    },
    Rect {
        start: PhysicalPoint,
        current: PhysicalPoint,
        color: Color,
        thickness: u32,
        fill: Option<Color>,
    },
}

impl EditorSession {
    pub fn new(document: CaptureDocument) -> Self {
        Self {
            document,
            selected_tool: AnnotationTool::Pen,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            active: None,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// The annotation currently being drawn, if any (for frontend preview).
    pub fn active_preview(&self) -> Option<Annotation> {
        match &self.active {
            Some(ActiveAnnotation::Pen {
                points,
                color,
                thickness,
            }) if points.len() >= 2 => Some(Annotation::Pen(PenStroke {
                color: *color,
                thickness: *thickness,
                points: points.clone(),
            })),
            Some(ActiveAnnotation::Rect {
                start,
                current,
                color,
                thickness,
                fill,
            }) => {
                let rect = PhysicalRect::from_points(*start, *current);
                if rect.is_empty() {
                    None
                } else {
                    Some(Annotation::Rectangle(RectShape {
                        rect,
                        color: *color,
                        thickness: *thickness,
                        fill: *fill,
                    }))
                }
            }
            _ => None,
        }
    }

    pub fn set_tool(&mut self, tool: AnnotationTool) {
        self.selected_tool = tool;
        self.active = None;
    }

    pub fn begin_annotation(&mut self, point: PhysicalPoint) {
        if self.document.crop.is_empty() {
            return;
        }
        let point = self.document.crop.clamp_point(point);
        self.active = Some(match self.selected_tool {
            AnnotationTool::Pen => ActiveAnnotation::Pen {
                points: vec![point],
                color: DEFAULT_PEN_COLOR,
                thickness: DEFAULT_PEN_THICKNESS,
            },
            AnnotationTool::Rectangle => ActiveAnnotation::Rect {
                start: point,
                current: point,
                color: DEFAULT_PEN_COLOR,
                thickness: DEFAULT_PEN_THICKNESS,
                fill: None,
            },
        });
    }

    pub fn update_annotation(&mut self, point: PhysicalPoint) {
        let point = self.document.crop.clamp_point(point);
        match &mut self.active {
            Some(ActiveAnnotation::Pen { points, .. }) => {
                if let Some(last) = points.last() {
                    if *last != point {
                        points.push(point);
                    }
                }
            }
            Some(ActiveAnnotation::Rect { current, .. }) => {
                *current = point;
            }
            None => {}
        }
    }

    /// Finalize the annotation. Returns `true` if a non-empty annotation was
    /// committed to the document.
    pub fn end_annotation(&mut self) -> bool {
        let finished = match self.active.take() {
            Some(ActiveAnnotation::Pen {
                points,
                color,
                thickness,
            }) => {
                if points.len() < 2 {
                    None
                } else {
                    Some(Annotation::Pen(PenStroke {
                        color,
                        thickness,
                        points,
                    }))
                }
            }
            Some(ActiveAnnotation::Rect {
                start,
                current,
                color,
                thickness,
                fill,
            }) => {
                let rect = PhysicalRect::from_points(start, current);
                if rect.is_empty() {
                    None
                } else {
                    Some(Annotation::Rectangle(RectShape {
                        rect,
                        color,
                        thickness,
                        fill,
                    }))
                }
            }
            None => None,
        };

        if let Some(annotation) = finished {
            self.record_history();
            self.document.push_annotation(annotation);
            true
        } else {
            false
        }
    }

    pub fn undo(&mut self) -> bool {
        if let Some(previous) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.restore(previous);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.restore(next);
            true
        } else {
            false
        }
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            crop: self.document.crop,
            annotations: self.document.annotations.clone(),
        }
    }

    fn restore(&mut self, snapshot: EditorSnapshot) {
        self.document.crop = snapshot.crop;
        self.document.annotations = snapshot.annotations;
        self.active = None;
    }

    fn record_history(&mut self) {
        self.undo_stack.push(self.snapshot());
        self.redo_stack.clear();
    }

    fn update_crop(&mut self, crop: PhysicalRect) -> bool {
        if self.document.crop == crop {
            return false;
        }
        self.record_history();
        self.document.crop = crop;
        true
    }
}

/// The capture session state machine.
#[derive(Debug, Clone)]
pub struct CaptureSession {
    state: CaptureSessionState,
    /// The raw captured frame, kept for document creation and frontend use.
    current_frame: Option<Arc<CapturedFrame>>,
    hover_candidate: Option<SnapCandidate>,
    timing: Timing,
}

impl Default for CaptureSession {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSession {
    pub fn new() -> Self {
        Self {
            state: CaptureSessionState::Idle,
            current_frame: None,
            hover_candidate: None,
            timing: Timing::default(),
        }
    }

    pub fn state(&self) -> &CaptureSessionState {
        &self.state
    }

    pub fn timing(&self) -> &Timing {
        &self.timing
    }

    pub fn frame(&self) -> Option<&CapturedFrame> {
        self.current_frame.as_deref()
    }

    pub fn hover_candidate(&self) -> Option<&SnapCandidate> {
        self.hover_candidate.as_ref()
    }

    /// Feed one command. Returns the events it produced.
    pub fn apply(&mut self, cmd: CaptureCommand) -> Vec<CaptureEvent> {
        // Take the state out so we can mutate `self` freely while stepping.
        let state = std::mem::replace(&mut self.state, CaptureSessionState::Idle);
        let (new_state, events) = self.step(state, cmd);
        self.state = new_state;
        events
    }

    fn step(
        &mut self,
        state: CaptureSessionState,
        cmd: CaptureCommand,
    ) -> (CaptureSessionState, Vec<CaptureEvent>) {
        match (state, cmd) {
            (CaptureSessionState::Idle, CaptureCommand::Begin) => {
                self.current_frame = None;
                self.hover_candidate = None;
                self.timing.reset();
                self.timing.mark_t0();
                (
                    CaptureSessionState::Preparing,
                    vec![CaptureEvent::StateChanged],
                )
            }
            (CaptureSessionState::Preparing, CaptureCommand::FrameReady(frame)) => {
                if let Err(error) = frame.validate() {
                    return (
                        CaptureSessionState::Preparing,
                        vec![CaptureEvent::Error(error)],
                    );
                }
                let frame = match frame.to_rgba8() {
                    Ok(frame) => frame,
                    Err(error) => {
                        return (
                            CaptureSessionState::Preparing,
                            vec![CaptureEvent::Error(error)],
                        )
                    }
                };
                self.timing.mark_t1();
                let bounds = frame.bounds();
                let mut sel = SelectionSession::new();
                sel.set_clamp_bounds(Some(bounds));
                sel.rect = bounds;
                self.current_frame = Some(Arc::new(frame));
                (
                    CaptureSessionState::Selecting(sel),
                    vec![CaptureEvent::StateChanged],
                )
            }
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::PointerMoved(p)) => {
                if sel.interaction == SelectionInteraction::Dragging {
                    sel.update_free_selection(p);
                    let rect = sel.rect;
                    (
                        CaptureSessionState::Selecting(sel),
                        vec![CaptureEvent::SelectionChanged(rect)],
                    )
                } else {
                    (CaptureSessionState::Selecting(sel), vec![])
                }
            }
            (CaptureSessionState::Selecting(sel), CaptureCommand::SnapCandidate(cand)) => {
                self.hover_candidate = cand.clone();
                let mut sel = sel;
                let mut events = vec![CaptureEvent::SnapCandidateChanged(cand.clone())];
                if let Some(c) = &cand {
                    if sel.interaction != SelectionInteraction::Dragging {
                        let next_rect = c.bounds.clamp(sel.clamp_bounds.unwrap_or_else(|| {
                            self.current_frame
                                .as_ref()
                                .map(|f| f.bounds())
                                .unwrap_or(c.bounds)
                        }));
                        if sel.rect != next_rect {
                            sel.rect = next_rect;
                            events.push(CaptureEvent::SelectionChanged(next_rect));
                        }
                        sel.interaction = SelectionInteraction::Hovering;
                    }
                } else {
                    if sel.interaction != SelectionInteraction::Dragging {
                        sel.set_idle();
                    }
                }
                (CaptureSessionState::Selecting(sel), events)
            }
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::BeginFreeSelection(p)) => {
                if sel.clamp_bounds.is_none() {
                    if let Some(f) = self.current_frame.as_ref() {
                        sel.set_clamp_bounds(Some(f.bounds()));
                    }
                }
                sel.begin_free_selection(
                    sel.clamp_bounds.map_or(p, |bounds| bounds.clamp_point(p)),
                );
                let rect = sel.rect;
                (
                    CaptureSessionState::Selecting(sel),
                    vec![CaptureEvent::SelectionChanged(rect)],
                )
            }
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::UpdateFreeSelection(p)) => {
                sel.update_free_selection(p);
                let rect = sel.rect;
                (
                    CaptureSessionState::Selecting(sel),
                    vec![CaptureEvent::SelectionChanged(rect)],
                )
            }
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::MoveSelection(delta)) => {
                sel.begin_move();
                sel.move_by(delta);
                let rect = sel.rect;
                (
                    CaptureSessionState::Selecting(sel),
                    vec![CaptureEvent::SelectionChanged(rect)],
                )
            }
            (
                CaptureSessionState::Selecting(mut sel),
                CaptureCommand::ResizeSelection(handle, p),
            ) => {
                sel.begin_resize(handle);
                sel.resize_to(handle, p);
                let rect = sel.rect;
                (
                    CaptureSessionState::Selecting(sel),
                    vec![CaptureEvent::SelectionChanged(rect)],
                )
            }
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::CommitSelection) => {
                let selection = sel.commit_selection();
                if selection.is_empty() || self.current_frame.is_none() {
                    return (
                        CaptureSessionState::Selecting(sel),
                        vec![CaptureEvent::Error(CaptureError::InvalidSelection(
                            "cannot commit an empty selection".to_string(),
                        ))],
                    );
                }
                let Some(frame) = self.current_frame.clone() else {
                    return (
                        CaptureSessionState::Selecting(sel),
                        vec![CaptureEvent::Error(CaptureError::InvalidSelection(
                            "cannot commit without a captured frame".to_string(),
                        ))],
                    );
                };
                let frame_bounds = frame.bounds();
                let document = CaptureDocument::new(frame, selection.clamp(frame_bounds));
                self.hover_candidate = None;
                (
                    CaptureSessionState::Editing(EditorSession::new(document)),
                    vec![CaptureEvent::StateChanged, CaptureEvent::DocumentChanged],
                )
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::SelectTool(tool)) => {
                editor.set_tool(tool);
                (
                    CaptureSessionState::Editing(editor),
                    vec![CaptureEvent::ToolChanged(tool)],
                )
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::MoveSelection(delta)) => {
                let crop = editor
                    .document
                    .crop
                    .translate(delta)
                    .clamp(editor.document.source.bounds());
                if editor.update_crop(crop) {
                    (
                        CaptureSessionState::Editing(editor),
                        vec![
                            CaptureEvent::SelectionChanged(crop),
                            CaptureEvent::DocumentChanged,
                        ],
                    )
                } else {
                    (CaptureSessionState::Editing(editor), vec![])
                }
            }
            (
                CaptureSessionState::Editing(mut editor),
                CaptureCommand::ResizeSelection(handle, p),
            ) => {
                let crop = resize_rect(
                    editor.document.crop,
                    handle,
                    p,
                    1,
                    Some(editor.document.source.bounds()),
                );
                if editor.update_crop(crop) {
                    (
                        CaptureSessionState::Editing(editor),
                        vec![
                            CaptureEvent::SelectionChanged(crop),
                            CaptureEvent::DocumentChanged,
                        ],
                    )
                } else {
                    (CaptureSessionState::Editing(editor), vec![])
                }
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::BeginAnnotation(p)) => {
                editor.begin_annotation(p);
                let events = if editor.active_preview().is_some() {
                    vec![CaptureEvent::DocumentChanged]
                } else {
                    vec![]
                };
                (CaptureSessionState::Editing(editor), events)
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::UpdateAnnotation(p)) => {
                let before = editor.active_preview();
                editor.update_annotation(p);
                let events = if before != editor.active_preview() {
                    vec![CaptureEvent::DocumentChanged]
                } else {
                    vec![]
                };
                (CaptureSessionState::Editing(editor), events)
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::EndAnnotation) => {
                let changed = editor.end_annotation();
                let events = if changed {
                    vec![CaptureEvent::DocumentChanged]
                } else {
                    vec![]
                };
                (CaptureSessionState::Editing(editor), events)
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::Undo) => {
                let changed = editor.undo();
                let events = if changed {
                    vec![CaptureEvent::DocumentChanged]
                } else {
                    vec![]
                };
                (CaptureSessionState::Editing(editor), events)
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::Redo) => {
                let changed = editor.redo();
                let events = if changed {
                    vec![CaptureEvent::DocumentChanged]
                } else {
                    vec![]
                };
                (CaptureSessionState::Editing(editor), events)
            }
            (CaptureSessionState::Editing(editor), CaptureCommand::InvokeAction(id)) => (
                CaptureSessionState::Editing(editor),
                vec![CaptureEvent::ActionRequested(id)],
            ),
            (
                CaptureSessionState::Selecting(_) | CaptureSessionState::Editing(_),
                CaptureCommand::Cancel,
            ) => {
                self.current_frame = None;
                self.hover_candidate = None;
                (
                    CaptureSessionState::Idle,
                    vec![CaptureEvent::StateChanged, CaptureEvent::Completed],
                )
            }
            (_, CaptureCommand::Cancel) => {
                self.current_frame = None;
                self.hover_candidate = None;
                (CaptureSessionState::Idle, vec![CaptureEvent::StateChanged])
            }
            // Out-of-order / invalid commands are reported as an error event.
            (rest, other) => (
                rest,
                vec![CaptureEvent::Error(CaptureError::InvalidCommand(format!(
                    "invalid command {:?}",
                    other
                )))],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_core::capture::PixelFormat;
    use capture_core::geometry::PhysicalSize;

    fn frame(w: u32, h: u32) -> CapturedFrame {
        let bytes = (w * h * 4) as usize;
        CapturedFrame::new(
            vec![0x80u8; bytes].into(),
            w,
            h,
            w * 4,
            PhysicalPoint::new(0, 0),
            PixelFormat::Rgba8,
        )
    }

    fn drive(session: &mut CaptureSession, cmds: Vec<CaptureCommand>) -> Vec<CaptureEvent> {
        cmds.into_iter().flat_map(|c| session.apply(c)).collect()
    }

    #[test]
    fn transitions_idle_to_editing_and_cancel() {
        let mut s = CaptureSession::new();
        let events = drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(100, 100)),
                CaptureCommand::BeginFreeSelection(PhysicalPoint::new(10, 10)),
                CaptureCommand::UpdateFreeSelection(PhysicalPoint::new(50, 50)),
                CaptureCommand::CommitSelection,
            ],
        );
        assert!(matches!(s.state(), CaptureSessionState::Editing(_)));
        assert!(events
            .iter()
            .any(|e| matches!(e, CaptureEvent::StateChanged)));
        assert!(events
            .iter()
            .any(|e| matches!(e, CaptureEvent::SelectionChanged(_))));

        // Cancel returns to Idle and emits Completed.
        let ev = s.apply(CaptureCommand::Cancel);
        assert!(matches!(s.state(), CaptureSessionState::Idle));
        assert!(ev.iter().any(|e| matches!(e, CaptureEvent::Completed)));
    }

    #[test]
    fn commit_default_full_frame_selection() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(200, 100)),
                CaptureCommand::CommitSelection,
            ],
        );
        match s.state() {
            CaptureSessionState::Editing(ed) => {
                assert_eq!(
                    ed.document.crop,
                    PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(200, 100))
                );
            }
            other => panic!("expected Editing, got {:?}", other),
        }
    }

    #[test]
    fn snap_candidate_previews_then_commits() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(400, 400)),
            ],
        );
        let candidate = SnapCandidate {
            id: capture_core::SnapCandidateId::new(7),
            bounds: PhysicalRect::new(PhysicalPoint::new(20, 30), PhysicalSize::new(100, 60)),
            kind: capture_core::SnapKind::Window,
            label: Some("win".to_string()),
            z_order: 0,
        };
        s.apply(CaptureCommand::SnapCandidate(Some(candidate.clone())));
        match s.state() {
            CaptureSessionState::Selecting(sel) => {
                assert_eq!(sel.rect, candidate.bounds);
            }
            other => panic!("expected Selecting, got {:?}", other),
        }
        s.apply(CaptureCommand::CommitSelection);
        match s.state() {
            CaptureSessionState::Editing(ed) => {
                assert_eq!(ed.document.crop, candidate.bounds);
            }
            other => panic!("expected Editing, got {:?}", other),
        }
    }

    #[test]
    fn snap_candidate_switch_updates_hovered_selection() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(400, 400)),
            ],
        );
        let first = SnapCandidate {
            id: capture_core::SnapCandidateId::new(1),
            bounds: PhysicalRect::new(PhysicalPoint::new(10, 20), PhysicalSize::new(80, 60)),
            kind: capture_core::SnapKind::Window,
            label: Some("first".to_string()),
            z_order: 0,
        };
        let second = SnapCandidate {
            id: capture_core::SnapCandidateId::new(2),
            bounds: PhysicalRect::new(PhysicalPoint::new(200, 220), PhysicalSize::new(100, 70)),
            kind: capture_core::SnapKind::Window,
            label: Some("second".to_string()),
            z_order: 1,
        };
        s.apply(CaptureCommand::SnapCandidate(Some(first)));
        s.apply(CaptureCommand::SnapCandidate(Some(second.clone())));
        match s.state() {
            CaptureSessionState::Selecting(selection) => assert_eq!(selection.rect, second.bounds),
            other => panic!("expected Selecting, got {:?}", other),
        }
    }

    #[test]
    fn annotation_append_undo_redo() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(200, 200)),
                CaptureCommand::CommitSelection,
            ],
        );
        // Draw a rectangle annotation.
        drive(
            &mut s,
            vec![
                CaptureCommand::SelectTool(AnnotationTool::Rectangle),
                CaptureCommand::BeginAnnotation(PhysicalPoint::new(10, 10)),
                CaptureCommand::UpdateAnnotation(PhysicalPoint::new(90, 90)),
                CaptureCommand::EndAnnotation,
            ],
        );
        let annotations = match s.state() {
            CaptureSessionState::Editing(ed) => ed.document.annotations.clone(),
            _ => panic!("expected Editing"),
        };
        assert_eq!(annotations.len(), 1);

        s.apply(CaptureCommand::Undo);
        let after_undo = match s.state() {
            CaptureSessionState::Editing(ed) => ed.document.annotations.len(),
            _ => panic!("expected Editing"),
        };
        assert_eq!(after_undo, 0);

        s.apply(CaptureCommand::Redo);
        match s.state() {
            CaptureSessionState::Editing(ed) => assert_eq!(ed.document.annotations.len(), 1),
            _ => panic!("expected Editing"),
        }
    }

    #[test]
    fn editing_selection_can_move_and_resize() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(200, 200)),
                CaptureCommand::BeginFreeSelection(PhysicalPoint::new(20, 20)),
                CaptureCommand::UpdateFreeSelection(PhysicalPoint::new(80, 80)),
                CaptureCommand::CommitSelection,
                CaptureCommand::MoveSelection(PhysicalPoint::new(10, 15)),
            ],
        );
        match s.state() {
            CaptureSessionState::Editing(ed) => {
                assert_eq!(
                    ed.document.crop,
                    PhysicalRect::new(PhysicalPoint::new(30, 35), PhysicalSize::new(60, 60))
                );
            }
            other => panic!("expected Editing, got {:?}", other),
        }
        s.apply(CaptureCommand::ResizeSelection(
            ResizeHandle::BottomRight,
            PhysicalPoint::new(120, 130),
        ));
        match s.state() {
            CaptureSessionState::Editing(ed) => {
                assert_eq!(ed.document.crop.right(), 120);
                assert_eq!(ed.document.crop.bottom(), 130);
            }
            other => panic!("expected Editing, got {:?}", other),
        }
        s.apply(CaptureCommand::Undo);
        match s.state() {
            CaptureSessionState::Editing(ed) => assert_eq!(ed.document.crop.right(), 90),
            other => panic!("expected Editing, got {:?}", other),
        }
        s.apply(CaptureCommand::Redo);
        match s.state() {
            CaptureSessionState::Editing(ed) => assert_eq!(ed.document.crop.right(), 120),
            other => panic!("expected Editing, got {:?}", other),
        }
    }

    #[test]
    fn annotations_are_clamped_to_crop() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(100, 100)),
                CaptureCommand::BeginFreeSelection(PhysicalPoint::new(20, 20)),
                CaptureCommand::UpdateFreeSelection(PhysicalPoint::new(60, 60)),
                CaptureCommand::CommitSelection,
                CaptureCommand::BeginAnnotation(PhysicalPoint::new(0, 0)),
                CaptureCommand::UpdateAnnotation(PhysicalPoint::new(100, 100)),
                CaptureCommand::EndAnnotation,
            ],
        );
        match s.state() {
            CaptureSessionState::Editing(ed) => match &ed.document.annotations[0] {
                Annotation::Pen(stroke) => {
                    assert_eq!(stroke.points[0], PhysicalPoint::new(20, 20));
                    assert_eq!(stroke.points[1], PhysicalPoint::new(59, 59));
                }
                other => panic!("expected pen annotation, got {:?}", other),
            },
            other => panic!("expected Editing, got {:?}", other),
        }
    }

    #[test]
    fn pen_stroke_is_committed() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![
                CaptureCommand::Begin,
                CaptureCommand::FrameReady(frame(200, 200)),
                CaptureCommand::CommitSelection,
            ],
        );
        drive(
            &mut s,
            vec![
                CaptureCommand::SelectTool(AnnotationTool::Pen),
                CaptureCommand::BeginAnnotation(PhysicalPoint::new(5, 5)),
                CaptureCommand::UpdateAnnotation(PhysicalPoint::new(20, 20)),
                CaptureCommand::UpdateAnnotation(PhysicalPoint::new(40, 40)),
                CaptureCommand::EndAnnotation,
            ],
        );
        match s.state() {
            CaptureSessionState::Editing(ed) => {
                assert_eq!(ed.document.annotations.len(), 1);
                assert!(matches!(ed.document.annotations[0], Annotation::Pen(_)));
            }
            other => panic!("expected Editing, got {:?}", other),
        }
    }

    #[test]
    fn invalid_command_reports_error() {
        let mut s = CaptureSession::new();
        // FrameReady before Begin is invalid.
        let ev = s.apply(CaptureCommand::FrameReady(frame(10, 10)));
        assert!(ev.iter().any(|e| matches!(e, CaptureEvent::Error(_))));
        assert!(matches!(s.state(), CaptureSessionState::Idle));
    }
}
