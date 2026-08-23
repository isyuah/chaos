# Application Runtime and Plugin Boundary

## Purpose

`capture-runtime` is the application orchestration layer above the existing
capture Core. It does not replace `capture-core` or the state machine in
`capture-annotation`.

The distinction is:

- Core owns screenshot domain rules, coordinates, selection, annotations,
  rendering payloads, and platform capture contracts.
- Runtime owns application settings, session lifecycle commands/events, and
  policies such as closing the overlay after a successful copy.
- The host owns event loops and side effects: Slint windows, clipboard access,
  global hotkeys, tray integration, settings persistence, and IPC transport.

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
on Linux. That adapter translates wire messages to the same `RuntimeCommand`
and `RuntimeEvent` types, but internal callers do not go through the wire path.
The existing `tools/capture-cli` remains a lower-level Core diagnostic tool;
it is not the user-facing application CLI described here.

## Action Flow

Built-in action IDs remain the static `capture_core::ActionId` values. A shell
button dispatches `CaptureCommand::InvokeAction`; the runtime emits
`RuntimeEvent::ActionRequested`; the host performs the platform side effect and
reports `RuntimeCommand::ActionCompleted`.

This keeps clipboard and window ownership out of the runtime while allowing the
runtime to apply settings after the result. For example, a successful Copy can
emit `CloseOverlay` when `copy_disposition` is `CloseOverlay`.

## Plugin Preparation

Trusted in-process plugins implement `RuntimePlugin`. They receive runtime
events and return runtime commands. A `PluginRegistry` ensures plugin IDs are
unique and routes owned `PluginActionId` values.

Plugin action IDs deliberately use owned strings. The Core's static `ActionId`
continues to represent built-in actions and is not stretched into a dynamic
plugin identifier.

A host drives plugins as follows:

```text
RuntimeEvent
  ├─→ application shell
  └─→ PluginRegistry::dispatch_event
          └─ RuntimeCommand(s) ─→ CaptureRuntime::dispatch
```

The current boundary does not promise a stable Rust dynamic-library ABI. It
also does not yet define untrusted-plugin permissions, process isolation,
manifest persistence, crash recovery, or protocol version negotiation. These
must be designed together when a real plugin is introduced. An external plugin
host can later expose the same semantic commands/events over a versioned IPC
protocol without changing the Slint integration.

## Next Integration Steps

1. Add a settings repository in the application host and load `AppSettings` at
   startup.
2. Keep the process alive and register platform global hotkeys; hotkey events
   dispatch `BeginCapture`.
3. Change cancel/complete behavior from quitting the Slint event loop to hiding
   and resetting the overlay for the next session.
4. Add a settings window that updates runtime settings through `SetSettings`.
5. Introduce one real plugin use case before defining manifests, permissions,
   or an IPC wire format.
