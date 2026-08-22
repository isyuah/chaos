# ADR-0003: Where the capture-session state machine lives

- Status: Accepted (for `demo-core-v1`)
- Date: (frozen with tag `demo-core-v1`)

## Context

The shared spec puts "selection domain" conceptually under Core, and the prompt
requires a centralized state machine (`CaptureSessionState`,
`CaptureCommand`, `CaptureEvent`). But the `Editing(EditorSession)` state owns a
`CaptureDocument`, whose `annotations: Vec<Annotation>` naturally lives in
`capture-annotation`. `capture-annotation` depends on `capture-core` (geometry,
`CapturedFrame`), so `capture-core` cannot depend back on `capture-annotation`
without creating a cycle.

## Decision

Place the full state machine (`CaptureCommand`, `CaptureEvent`,
`CaptureSessionState`, `CaptureSession`) in **`capture-annotation`**, alongside
`CaptureDocument`.

`capture-core` still owns the pure selection *geometry* primitives
(`SelectionSession`, `SelectionTool`, resize handle ids) and the
`CapturedFrame`/`SnapCandidate` data types, so a frontend can consume them
without pulling the annotation model.

## Consequences

- The dependency diagram stays acyclic and correct: `annotation → core`, no edge
  `core → annotation`.
- `capture-core` remains toolkit- and document-free (as the demo's "domain" crate
  for geometry + capture + snap + placement).
- The session is *testable without a backend*: it consumes a `CapturedFrame` via
  `CaptureCommand::FrameReady`, never calling `CaptureBackend` itself, so the
  driver (frontend or CLI) owns platform capture and the session stays pure.
- Frontends interact with exactly one entry point (`CaptureSession`) that exposes
  the current `CaptureSessionState` and accepts commands.
