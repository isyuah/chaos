# Application Runtime and Plugin Boundary

## Purpose

`capture-runtime` is the application orchestration layer above the existing
capture Core. It does not replace `capture-core` or the state machine in
`capture-annotation`.

The distinction is:

- Core owns screenshot domain rules, coordinates, selection, annotations,
  rendering payloads, and platform capture contracts.
- Runtime owns session lifecycle, pending action requests, and policies such as
  closing the overlay after a successful copy.
- The host owns event loops and side effects: Slint windows, clipboard access,
  global hotkeys, save paths, capture sources, tray integration, application
  settings, persistence, and IPC transport.

## One Process, Direct Calls

The resident application and Slint UI remain in one process. They call
`CaptureRuntime::dispatch` directly; internal calls are not serialized through
standard input, command-line arguments, or IPC.

```text
global hotkey ─┐
settings UI ───┼─ direct RuntimeCommand ─→ CaptureRuntime
Slint overlay ─┘                              │
                                             └─ RuntimeEvent ─→ host effects
```

An external process may later use Named Pipes on Windows or Unix sockets / D-Bus
on Linux. A transport adapter translates versioned wire requests into internal
runtime operations. The Rust `RuntimeCommand` and `RuntimeEvent` enums are an
evolving in-process model, not a stable or serializable protocol.
The existing `tools/capture-cli` remains a lower-level Core diagnostic tool;
it is not the user-facing application CLI described here.

## Action Flow

Built-in action IDs remain the static `capture_core::ActionId` values. A shell
button dispatches `CaptureCommand::InvokeAction`; the runtime emits
`RuntimeEvent::ActionRequested { request_id, action }`; the host performs the
platform side effect and reports `RuntimeCommand::CompleteAction` with the same
`ActionRequestId`. Unknown, stale, or already completed request IDs are rejected.

This keeps clipboard and window ownership out of the runtime while allowing the
runtime to apply policy after the result. For example, a successful Copy can
emit `CloseOverlay` when `copy_disposition` is `CloseOverlay`.

## Capture Flow

`BeginCapture` creates a new `CaptureSessionId` and emits `CaptureRequested`.
The host hides the overlay, performs capture on a worker thread, and returns
either `FrameReady { session_id, frame }` or
`FrameFailed { session_id, message }`. Results for an older or already closed
session are rejected and cannot replace the active capture. The overlay is
shown only after the matching frame has been accepted.

The Slint host keeps the event loop resident while all windows are hidden. It
owns global hotkeys, tray integration, capture workers, and explicit shutdown;
Cancel resets and hides the current overlay without terminating the process.

## Plugin Preparation

Only declarative `PluginDescriptor` and owned `PluginActionId` types are kept at
this stage. The Core's static `ActionId` continues to represent built-in actions
and is not stretched into a dynamic plugin identifier.

There is intentionally no plugin execution trait or event subscription API yet.
The first real plugin must determine what document/image context it receives,
which effects it may request, whether it runs in-process or out-of-process, and
how permissions, cancellation, timeouts, crashes, and protocol versions work.
Plugins will not receive arbitrary access to `RuntimeCommand`.

## Next Integration Steps

1. Add a settings repository in the host and pass only `RuntimePolicy` into the
   runtime.
2. Make the default shortcut configurable and persist platform registration
   failures as actionable settings diagnostics.
3. Introduce one real plugin use case before defining manifests, permissions,
   or an IPC wire format.
