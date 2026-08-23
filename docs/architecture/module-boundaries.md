# Module Boundaries

This document is the authoritative description of the shared Rust Core crate
graph. It is intentionally written before the code so that the two UI frontends
(`capture-slint`, `capture-qml`) can rely on a stable contract.

## 1. Dependency direction

```text
platform implementations (capture-windows, capture-linux)
        ↓
capture-platform-api            (traits only, no platform impls)
        ↓
capture-core                   (geometry, capture/snap data types, placement)
     ↓
capture-annotation
     ├──────────────→ capture-runtime
     ├──────────────→ capture-render
     └──────────────→ capture-actions
```

The actual crate dependency edges are:

| Crate | Depends on |
|---|---|
| `capture-core` | *(nothing — std + `thiserror` only)* |
| `capture-platform-api` | `capture-core` |
| `capture-annotation` | `capture-core` |
| `capture-runtime` | `capture-core`, `capture-annotation` |
| `capture-render` | `capture-core`, `capture-annotation` |
| `capture-actions` | `capture-core`, `capture-annotation`, `capture-render` |
| `capture-windows` | `capture-core`, `capture-platform-api`, `windows` |
| `capture-linux` | `capture-core`, `capture-platform-api` |
| `tools/capture-cli` | Core crates, platform adapters (diagnostic/acceptance path) |
| `apps/capture-slint` | runtime, Core crates, platform adapters, Slint, clipboard bridge |

No crate in this graph depends on a UI toolkit (Slint, Qt/QML, winit, tao).
The last statement applies to the shared Core graph; `apps/capture-slint` is the
explicit frontend shell and is intentionally outside that graph.
Platform native types (`HWND`, X11/`Wayland` handles) never escape the
`capture-windows` / `capture-linux` crates; they are converted to opaque
`u64`/`PhysicalPoint` values at the API boundary.

## 2. Crate responsibilities

### `capture-core`
UI-neutral, platform-neutral, toolkit-free. The canonical coordinate space is
**physical pixels**.

- `geometry` — `PhysicalPoint`, `PhysicalSize`, `PhysicalRect`, `ScaleFactor`,
  plus `intersection`, `clamp`, `translate`, `contains`, `inflate`, `normalize`,
  and negative-virtual-coordinate helpers.
- `coord` — `CoordinateMapper`, `VirtualDesktopMapper`, and the physical↔logical
  contract for mixed-DPI.
- `capture` — `PixelFormat`, `CapturedFrame`, `MonitorId`, `MonitorInfo`
  (including work area), `CaptureCapabilities`, `CaptureError`.
- `snap` — `SnapKind`, `SnapCandidateId`, `SnapCandidate`, `SnapCapabilities`,
  `SnapError`, `SnapExclusionToken`.
- `placement` — `place_toolbar`, `ToolbarPlacement`, `ToolbarPlacementReason`.
- `selection` — pure selection geometry state (`SelectionSession`,
  `SelectionTool`, resize handle ids). Contains **no** annotation document.

### `capture-platform-api`
The formal abstraction surface. Defines `CaptureBackend` and `SnapBackend`
traits, including optional atomic virtual-desktop capture. Uses only
`capture-core` types. No implementation code.

### `capture-annotation`
The annotation document (`Annotation`, `PenStroke`, `RectShape`,
`CaptureDocument`), the undo stack, and the **capture session state machine**
(`CaptureCommand`, `CaptureEvent`, `CaptureSessionState`, `CaptureSession`).

> Rationale for the session being here rather than in `capture-core`: the
> `Editing(EditorSession)` state must hold a `CaptureDocument`, whose
> `annotations: Vec<Annotation>` lives in this crate. Placing the session here
> preserves the rule that `capture-core` never depends on `capture-annotation`
> (which would create a cycle), while still leaving the whole diagram
> dependency-correct (`annotation → core`).

### `capture-render`
Flattens a `CaptureDocument` (source frame + crop + annotations) into a final
RGBA8 bitmap, and encodes it to PNG. Golden tests live here.

### `capture-runtime`

Owns the application-level session driver and policy above the capture domain:

- `AppSettings` and behavior such as whether a successful copy closes the overlay;
- `RuntimeCommand` / `RuntimeEvent`, shared by the resident app, settings UI,
  future hotkey adapters, application CLI integration, and optional IPC;
- the trusted in-process `RuntimePlugin` boundary and plugin action registry.

It is UI- and platform-neutral. It does not create windows, register global
hotkeys, write the clipboard, persist settings, or dynamically load plugins.
Those are host responsibilities. See `runtime-and-plugins.md`.

The existing `tools/capture-cli` intentionally remains a low-level diagnostic
and acceptance tool that can exercise Core and platform adapters directly. A
future user-facing application CLI should call `capture-runtime` instead.

### `capture-actions`
`CaptureAction` trait plus `Copy`, `Save`, `Pin`, `AskAi` actions. Actions only
**produce payloads** (bytes + metadata); writing to the OS clipboard or creating
a Pin window is deferred to the caller (frontend shell) exactly as the spec
requires — Core never touches a UI event loop.

### `capture-windows`
Implements `CaptureBackend` (GDI `BitBlt` screen capture) and `SnapBackend`
(visible top-level-window enumeration + point hit-testing). All `unsafe` and
all `HWND`/`HDC` handling is confined here.

### `capture-linux`
Provides the same two traits behind `#[cfg(target_os = "linux")]`. X11 uses
RandR 1.5 monitor enumeration, root-window `GetImage` capture, and EWMH client
stacking/window geometry for snap candidates. Native Wayland uses the XDG
ScreenCast portal and a short-lived PipeWire consumer to obtain an authorized
single frame. Pure Wayland has no global window-list protocol, so window-level
snap remains unavailable there; XWayland continues to use the X11 path.

## 3. What each frontend implements (NOT in Core)

Per `00_DEMO_COMMON_SPEC.md` §17, the following are experiment variables and
intentionally absent from Core:

- window creation, transparency / frameless / topmost integration
- native window handle conversion (`*mut c_void` → toolkit handle)
- multi-window lifecycle, Pin Window
- image bridge: `CapturedFrame` → toolkit image/texture
- toolbar rendering, selection visual rendering, annotation preview rendering
- animation, input dispatch adapter, first-present measurement

## 4. Contract for adding a frontend

A frontend:

1. Uses the `apps/capture-slint` package (or creates a future frontend package),
   adds it to the workspace member list, and depends on
   `capture-runtime`/`capture-core`/`capture-platform-api`/
   `capture-annotation`/`capture-render`/`capture-actions`.
2. Selects a backend: `capture-windows::WindowsPlatform` on Windows,
   `capture-linux::LinuxPlatform` elsewhere (the CLI's `platform` module shows
   the one-line wiring).
3. Consumes `CaptureBackend`/`SnapBackend` through `&dyn` (they are object-safe).
4. Passes all native overlay and Pin window handles into
   `SnapBackend::set_excluded_windows` as `SnapExclusionToken` values. The
   singular setter is retained for one-window integrations.

If the frozen Core API blocks a reasonable implementation, the frontend
follows `CORE_CHANGE_REQUEST.md` instead of silently forking Core.
