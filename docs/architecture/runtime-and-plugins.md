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

## Settings Ownership

The Slint host owns a versioned JSON settings document in the platform config
directory. It persists the global shortcut, screenshot directory, successful
copy behavior, and the latest shortcut registration diagnostic. Writes use a
temporary file in the same directory followed by an atomic replacement. A
missing file selects defaults; an unreadable, malformed, or newer-schema file
is reported and left untouched instead of being silently replaced.

Only `RuntimePolicy` crosses into `capture-runtime`. Paths, shortcut syntax,
folder dialogs, and platform registration remain host concerns. Windows and
X11 replace a shortcut transactionally by retaining the old registration until
the new one succeeds. On Wayland the configured shortcut is a preferred trigger
for a new XDG GlobalShortcuts portal session; the compositor remains the final
authority and may ask the user to approve or change it. Portal registration
failures are returned to the settings UI and persisted for the next launch.

## Plugin Preparation

Only declarative `PluginDescriptor` and owned `PluginActionId` types are kept at
this stage. The Core's static `ActionId` continues to represent built-in actions
and is not stretched into a dynamic plugin identifier.

There is intentionally no plugin execution trait or event subscription API yet.
The first real plugin must determine what document/image context it receives,
which effects it may request, whether it runs in-process or out-of-process, and
how permissions, cancellation, timeouts, crashes, and protocol versions work.
Plugins will not receive arbitrary access to `RuntimeCommand`.

OCR is the intended first vertical slice. Its actual engine and product flow
must first establish whether it needs the flattened crop, language hints,
progress/cancellation, clipboard access, network access, or a structured text
result. Only then should the host introduce a `PluginActionContext`, bounded
result types, and explicit permissions. This avoids accidentally turning a
temporary DLL, WASM, or IPC experiment into the permanent plugin ABI.

## Next Integration Steps

1. Complete interactive acceptance of settings and shortcut replacement on a
   real Windows desktop and both X11 and Wayland Linux sessions.
2. Implement one OCR vertical slice and use its observed requirements to define
   the first execution context, result, cancellation, and permission contracts.
3. Choose in-process, DLL, WASM, or IPC isolation only after the OCR engine and
   its deployment constraints are known.
