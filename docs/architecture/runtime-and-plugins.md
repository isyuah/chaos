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

1. Keep the process alive and make `BeginCapture` request a new host capture.
2. Add capture session IDs before capture work becomes asynchronous.
3. Add a settings repository in the host and pass only `RuntimePolicy` into the
   runtime.
4. Register platform global hotkeys; hotkey events dispatch `BeginCapture`.
5. Change cancel/complete behavior from quitting the Slint event loop to hiding
   and resetting the overlay for the next session.
6. Introduce one real plugin use case before defining manifests, permissions,
   or an IPC wire format.
