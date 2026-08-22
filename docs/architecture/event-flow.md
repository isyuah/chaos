# Event Flow

This document describes the command/event flow through the shared Core
(`capture-annotation`'s `CaptureSession`). The frontend is the *driver*: it owns
pointer/keyboard events and calls commands; the Core owns the **state** and
mutates the annotation document. The frontend subscribes to events and decides
how to render.

## 1. Driver model

The frontend (or the CLI) is responsible for:

- capturing the frame (`CaptureBackend::capture_monitor`) and delivering it as
  `CaptureCommand::FrameReady(CapturedFrame)`;
- translating OS pointer events into `PhysicalPoint`s (already in physical
  pixels);
- interpreting snap candidates from `SnapBackend::candidates_at` and telling the
  Core which candidate is hovered (`CaptureCommand::SnapCandidate`) so it can
  highlight and update the preview rectangle;
- drawing all visual feedback (selection rect, handles, annotation preview,
  toolbar) from the state the Core exposes.

The Core is responsible for: transition validity, selection geometry mutation,
document/annotation mutation, undo, action invocation, and reporting
`CaptureEvent`s.

## 2. State machine

```text
 Idle
  │ Begin
  ▼
 Preparing
  │ FrameReady
  ▼             ┌──────────────────────────────┐
 Selecting ─────┤ CommitSelection               │
  │             ▼                              │
  │           Editing ──(Move/Resize/Undo/Redo/InvokeAction)──► Editing
  │             │                              │
  └── Cancel ◄──┴── Cancel                     │
      │                              Completed │
 Idle ◄─────────────────────────────────────────┘
```

## 3. Commands (input, `CaptureCommand`)

| Command | Valid in | Effect |
|---|---|---|
| `Begin` | Idle | → `Preparing` |
| `FrameReady(frame)` | Preparing | → `Selecting` (store `CapturedFrame`, default crop = full frame) |
| `PointerMoved(p)` | Selecting | update hover candidate / free-drag preview |
| `BeginFreeSelection(p)` | Selecting | start free drag |
| `UpdateFreeSelection(p)` | Selecting | update dragging rect |
| `CommitSelection` | Selecting | → `Editing` (normalize crop, copy region into doc) |
| `MoveSelection(delta)` | Editing | move crop within source |
| `ResizeSelection(handle, target)` | Editing | resize crop via handle |
| `SelectTool(tool)` | Editing | set active tool |
| `Undo` | Editing | restore the previous crop + annotation snapshot |
| `Redo` | Editing | restore the next crop + annotation snapshot |
| `InvokeAction(action_id)` | Editing | emit `ActionRequested` |
| `Cancel` | Selecting/Editing | → `Idle`, discard doc |

## 4. Events (output, `CaptureEvent`)

- `StateChanged`
- `SnapCandidateChanged(Option<SnapCandidate>)`
- `SelectionChanged(PhysicalRect)`
- `DocumentChanged`
- `ToolChanged(AnnotationTool)`
- `ActionRequested(ActionId)`
- `Completed`
- `Error(CaptureError)`

Events carry **state**, never rendering detail. A frontend maps these to its own
drawing primitives.

## 5. Measurement timestamps

The Core records only `T0` (hotkey) and `T1` (frame ready) via
`capture_core::capture::Timing`. The frontend records `T2`/`T3`/`T4` and
combines them. `T0` must be sent before the backend capture call, as the CLI
acceptance flow does. See `CORE_BASELINE_REPORT.md` for the measurement notes.

## 6. Self-exclusion

Before it starts snapping, the frontend passes all overlay/Pin window handles to
`SnapBackend::set_excluded_windows(tokens)`, so none of its own windows can be
highlighted. `set_excluded_window(Some(token))` remains as a compatibility
shortcut for one window. Tokens are opaque `u64` values whose meaning is
backend-defined.
