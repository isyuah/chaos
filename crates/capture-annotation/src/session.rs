//! The centralized capture-session state machine.
//!
//! The frontend is the driver: it feeds [`CaptureCommand`]s (translated from OS
//! pointer/keyboard events + capture results) and consumes the [`CaptureEvent`]s
//! produced. The Core owns the state and the annotation document.

use crate::document::{Annotation, CaptureDocument, Color, PenStroke, RectShape};
use crate::tools::AnnotationTool;
use capture_core::action::ActionId;
use capture_core::capture::{CapturedFrame, Timing};
use capture_core::geometry::{PhysicalPoint, PhysicalRect};
use capture_core::selection::{ResizeHandle, SelectionInteraction, SelectionSession};
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
    ActionRequested(ActionId),
    Completed,
    Error(String),
}

const DEFAULT_PEN_THICKNESS: u32 = 3;
const DEFAULT_PEN_COLOR: Color = Color::RED;

/// The editing half of the state: the document plus undo/redo and the
/// in-progress annotation preview.
#[derive(Debug, Clone)]
pub struct EditorSession {
    pub document: CaptureDocument,
    pub selected_tool: AnnotationTool,
    undo_stack: Vec<Annotation>,
    redo_stack: Vec<Annotation>,
    active: Option<ActiveAnnotation>,
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
        !self.document.annotations.is_empty()
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
        if !self.document.crop.contains_exclusive(point) {
            return;
        }
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
            self.document.push_annotation(annotation);
            self.redo_stack.clear();
            true
        } else {
            false
        }
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.document.annotations.pop() {
            self.undo_stack.push(prev);
            self.redo_stack.clear();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.document.annotations.push(next);
            true
        } else {
            false
        }
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
                self.timing.mark_t0();
                (CaptureSessionState::Preparing, vec![CaptureEvent::StateChanged])
            }
            (CaptureSessionState::Preparing, CaptureCommand::FrameReady(frame)) => {
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
                if let Some(c) = &cand {
                    if !sel.is_active() || sel.interaction == SelectionInteraction::Idle {
                        sel.rect = c.bounds.clamp(
                            sel.clamp_bounds
                                .unwrap_or_else(|| self.current_frame.as_ref().map(|f| f.bounds()).unwrap_or(c.bounds)),
                        );
                        sel.interaction = SelectionInteraction::Hovering;
                    }
                } else {
                    sel.set_idle();
                }
                (
                    CaptureSessionState::Selecting(sel),
                    vec![CaptureEvent::SnapCandidateChanged(cand)],
                )
            }
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::BeginFreeSelection(p)) => {
                if sel.clamp_bounds.is_none() {
                    if let Some(f) = self.current_frame.as_ref() {
                        sel.set_clamp_bounds(Some(f.bounds()));
                    }
                }
                sel.begin_free_selection(p);
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
            (CaptureSessionState::Selecting(mut sel), CaptureCommand::ResizeSelection(handle, p)) => {
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
                        vec![CaptureEvent::Error("cannot commit an empty selection".to_string())],
                    );
                }
                let bounds = self.current_frame.as_ref().map(|f| f.bounds()).unwrap_or(selection);
                let document = CaptureDocument::new(
                    self.current_frame.clone().unwrap(),
                    selection.clamp(bounds),
                );
                (
                    CaptureSessionState::Editing(EditorSession::new(document)),
                    vec![CaptureEvent::StateChanged, CaptureEvent::DocumentChanged],
                )
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::SelectTool(tool)) => {
                editor.set_tool(tool);
                (
                    CaptureSessionState::Editing(editor),
                    vec![CaptureEvent::DocumentChanged],
                )
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
                editor.update_annotation(p);
                (
                    CaptureSessionState::Editing(editor),
                    vec![CaptureEvent::DocumentChanged],
                )
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::EndAnnotation) => {
                let _ = editor.end_annotation();
                (
                    CaptureSessionState::Editing(editor),
                    vec![CaptureEvent::DocumentChanged],
                )
            }
            (CaptureSessionState::Editing(mut editor), CaptureCommand::Undo) => {
                let _ = editor.undo();
                (
                    CaptureSessionState::Editing(editor),
                    vec![CaptureEvent::DocumentChanged],
                )
            }
            (CaptureSessionState::Editing(editor), CaptureCommand::InvokeAction(id)) => {
                (CaptureSessionState::Editing(editor), vec![CaptureEvent::ActionRequested(id)])
            }
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
                vec![CaptureEvent::Error(format!(
                    "invalid command {:?}",
                    other
                ))],
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
        assert!(matches!(
            s.state(),
            CaptureSessionState::Editing(_)
        ));
        assert!(events.iter().any(|e| matches!(e, CaptureEvent::StateChanged)));
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
                assert_eq!(ed.document.crop, PhysicalRect::new(PhysicalPoint::new(0, 0), PhysicalSize::new(200, 100)));
            }
            other => panic!("expected Editing, got {:?}", other),
        }
    }

    #[test]
    fn snap_candidate_previews_then_commits() {
        let mut s = CaptureSession::new();
        drive(
            &mut s,
            vec![CaptureCommand::Begin, CaptureCommand::FrameReady(frame(400, 400))],
        );
        let candidate = SnapCandidate {
            id: capture_core::SnapCandidateId::new(7),
            bounds: PhysicalRect::new(PhysicalPoint::new(20, 30), PhysicalSize::new(100, 60)),
            kind: capture_core::SnapKind::Window,
            label: Some("win".to_string()),
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
