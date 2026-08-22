# CORE_BASELINE_REPORT — capture-ui-eval shared Core (demo-core-v1)

Frozen commit: `4de8ddd0e21a84ad2a9ec7030fa89d67acd4b9fd`
Tag: `demo-core-v1`
Build environment: Windows 11, `rustc 1.97.1` (`x86_64-pc-windows-msvc`), `cargo 1.97.1`

---

## 0. Goal

A single, UI-neutral, platform-neutral **Rust Core** that two independent UI
frontends (`capture-slint`, `capture-qml`) build against, so that the only
experimental variables are the UI toolkits themselves. This document records the
frozen baseline, the real Windows verification, the known issues, and exactly how
a frontend plugs in.

---

## 1. Crate graph

```text
crates/capture-core            (geometry, capture/snap data types, placement, ActionId)
        ▲
crates/capture-platform-api    (CaptureBackend + SnapBackend traits)
        ▲
   ┌────┴─────────┬──────────────┐
capture-windows  capture-linux   (platform implementations, isolated)
        ▲              ▲
        └──────┬───────┘
   capture-annotation  capture-actions
        ▲
   capture-render
        ▲
tools/capture-cli      (the frozen-Core acceptance tool)
```

Dependency edges (each crate's `Cargo.toml [dependencies]`):

| crate | depends on |
|---|---|
| `capture-core` | *(std + `thiserror` only)* |
| `capture-platform-api` | `capture-core` |
| `capture-annotation` | `capture-core` |
| `capture-render` | `capture-core`, `capture-annotation`, `png` |
| `capture-actions` | `capture-core`, `capture-annotation`, `capture-render` |
| `capture-windows` | `capture-core`, `capture-platform-api`, `windows` |
| `capture-linux` | `capture-core`, `capture-platform-api` |
| `tools/capture-cli` | all of the above, `clap`, `arboard` |

No crate depends on a UI toolkit (Slint, Qt, winit, tao). See
`docs/architecture/module-boundaries.md`.

### Where the session lives
The centralized state machine (`CaptureCommand` / `CaptureEvent` /
`CaptureSession`) lives in `capture-annotation` with the document, not in
`capture-core`, to avoid a `core → annotation` cycle. The rationale and
tradeoffs are recorded in `docs/adr/0003-session-crate-placement.md`.

---

## 2. Core API (what the frontends consume)

All public types are in `capture_core` (re-exports), `capture_platform_api`,
`capture_annotation`, `capture_render`, `capture_actions`.

### Geometry & coordinate (physical pixels are canonical; negative allowed)
```rust
PhysicalPoint { x: i32, y: i32 }
PhysicalSize  { width: u32, height: u32 }
PhysicalRect  { origin: PhysicalPoint, size: PhysicalSize }
ScaleFactor(pub f64)
LogicalPoint / LogicalSize / LogicalRect   // toolkit space only, at the bridge
CoordinateMapper { scale_factor, physical_origin }
```

Operations: `from_points` (normalizes drag), `contains` / `contains_exclusive`,
`intersection`, `intersects`, `clamp`, `translate`, `inflate`,
`union`, `inset`, `center`, `right`/`bottom`/`left`/`top`.

### Capture domain
```rust
pub struct CapturedFrame {
    pub pixels: Arc<[u8]>,
    pub width: u32, pub height: u32, pub stride: u32,
    pub origin: PhysicalPoint,
    pub pixel_format: PixelFormat,
}
pub struct MonitorInfo { id, name, bounds, scale_factor, is_primary }
pub struct MonitorId(pub u32);
pub enum PixelFormat { Rgba8, Bgra8, Rgb24, U8 }
pub struct Timing { t0_hotkey_received, t1_frame_ready, ... }
```

### Snap domain
```rust
pub struct SnapCandidate { id, bounds, kind, label }
pub enum SnapKind { Window, Desktop }
pub struct SnapCandidateId(pub u64);
pub struct SnapExclusionToken(pub u64);
pub fn rank_candidates(point, candidates) -> Vec<SnapCandidate>;
```

### Toolbar placement
```rust
pub fn place_toolbar(selection, toolbar_size, work_area, preferred_gap) -> ToolbarPlacement;
pub enum ToolbarPlacementReason { Below, Above, InsideBottom, Clamped }
```

### Backends (`capture-platform-api`)
```rust
pub trait CaptureBackend: Send + Sync {
    fn capabilities(&self) -> CaptureCapabilities;
    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;
    fn capture_monitor(&self, id: MonitorId) -> Result<CapturedFrame, CaptureError>;
}
pub trait SnapBackend: Send + Sync {
    fn capabilities(&self) -> SnapCapabilities;
    fn candidates_at(&self, point: PhysicalPoint) -> Result<Vec<SnapCandidate>, SnapError>;
    fn set_excluded_window(&self, token: Option<SnapExclusionToken>) { let _ = token; }
}
```

### Session (`capture-annotation`)
```rust
pub enum CaptureSessionState { Idle, Preparing, Selecting(SelectionSession), Editing(EditorSession) }
pub enum CaptureCommand { Begin, FrameReady(CapturedFrame), PointerMoved, SnapCandidate,
                          BeginFreeSelection, UpdateFreeSelection, CommitSelection,
                          MoveSelection, ResizeSelection, SelectTool, BeginAnnotation,
                          UpdateAnnotation, EndAnnotation, Undo, InvokeAction(ActionId), Cancel }
pub enum CaptureEvent { StateChanged, SelectionChanged, SnapCandidateChanged,
                        DocumentChanged, ActionRequested, Completed, Error }
pub struct CaptureSession;   // .apply(cmd) -> Vec<CaptureEvent>; .state(); .frame(); .timing()
pub struct EditorSession { document, selected_tool, undo/redo, active_preview() }
```

### Render & actions
```rust
pub fn flatten(&CaptureDocument) -> Result<RenderedImage, RenderError>;
pub fn encode_png(&RenderedImage) -> Result<Vec<u8>, RenderError>;
pub fn save_png(path, &RenderedImage) -> Result<(), RenderError>;
pub fn checksum(&RenderedImage) -> u64;   // golden-test reference

pub trait CaptureAction: Send + Sync {
    fn id(&self) -> &'static str;
    fn invoke(&self, &CaptureDocument) -> Result<ActionOutcome, ActionError>;
}
// CopyAction, SaveAction{path}, PinAction, AskAiAction
// ActionOutcome::{ Png(ActionPayload), Saved(PathBuf), Pin(ActionPayload), AskAi(ActionPayload) }
```

---

## 3. Geometry / coordinate / mixed-DPI

- Canonical space is physical pixels; `PhysicalRect` origins may be negative
  (needed when a monitor sits left/above the primary).
- `CoordinateMapper` is per-monitor; physical↔logical conversion is pure
  arithmetic (logical types appear only at the UI bridge).
- `capture-windows` sets **Per-Monitor-V2 DPI awareness** at construction so
  `GetDC(NULL)` / `EnumDisplayMonitors` report physical pixels and physical rects.
- Unit tests cover negative virtual coordinates, mixed-drag normalization,
  clamp/translate/inflate/union, and intersect. `docs/architecture/coordinate-system.md`.

### Real-machine observation (this host)
`capture-cli monitors` (real, dual-monitor, mixed-DPI):

```
[0] \\.\DISPLAY3 bounds=(-2560,0 2560x1440) scale=1.5 primary=false
[1] \\.\DISPLAY2 bounds=(0,0 2560x1440) scale=1.0 primary=true
```

This is exactly the hard case the spec (§15) asks for: a **negative-origin
secondary monitor** (x = -2560) at **150% DPI** beside a 100% primary. The
backend returns correct physical bounds and per-monitor scale factors, and a
capture of monitor 0 produced `2560x1440 origin=(-2560,0)`.

---

## 4. Windows capture implementation

- **Backend:** GDI `BitBlt` (ADR-0001). The whole virtual desktop is captured
  once into a compatible bitmap, read back top-down with `GetDIBits`
  (`biHeight` negative), then cropped to the requested monitor rect and
  converted BGRA→RGBA8.
- `monitors()` enumerates via `EnumDisplayMonitors` + `GetMonitorInfoW`
  (device name from `MONITORINFOEXW`); scale from `GetDpiForMonitor(…, MDT_EFFECTIVE_DPI)`.
- `capture_monitor(id)` yields a `CapturedFrame` whose `origin` equals the
  monitor's virtual top-left (negative-origin aware).
- **Why GDI (not WGC/DXGI):** no D3D device, no message loop, no WinRT
  activation; reliable across all Windows/DPI configs for a CLI or headless test.
  DXGI/WGC migration is isolated behind the same `CaptureBackend` trait.
- **Windows snap:** `EnumWindows` top-level window enumeration → filter
  `IsWindowVisible`, exclude tool windows (`WS_EX_TOOLWINDOW`), exclude the
  self window via the exclusion token, keep windows under the point, read window
  text as the label, and append a virtual-desktop fallback candidate. Ordering
  follows OS Z-order; `rank_candidates` is the shared fallback rule.
- All `unsafe` and all `HWND`/`HDC`/`HBITMAP` handling is confined to
  `capture-windows`.

---

## 5. Linux implementation status

- `capture-linux` implements both traits and **cross-checks cleanly** for
  `x86_64-unknown-linux-gnu` (`cargo check --target x86_64-unknown-linux-gnu -p capture-linux`).
- For the demo it is a **compilable backend route**, not a production capture
  path: `monitors`/`capture_monitor`/`candidates_at` return
  `CaptureError::Unsupported` / `SnapError::Unsupported` with a clear message so
  a frontend gets a deterministic "not yet implemented here" rather than a crash.
- **Documented production route** (behind the same traits): X11 via XRandR +
  XShm capture and `_NET_CLIENT_LIST` window querying; Wayland via the XDG
  ScreenCast portal (`org.freedesktop.portal.ScreenCast`) for capture and the
  compositor window API for snapping. Wayland's global-window-positioning and
  always-on-top limitations are noted in `00_DEMO_COMMON_SPEC.md` §16.

---

## 6. Native dependencies

| crate | native dep | scope |
|---|---|---|
| `capture-windows` | `windows` 0.61 (features: `Win32_Foundation`, `Win32_Graphics_Gdi`, `Win32_UI_HiDpi`, `Win32_UI_WindowsAndMessaging`) | GDI capture, DPI awareness, window enumeration |
| `capture-linux` | *(none; pure-Rust skeleton)* | — |
| `tools/capture-cli` | `arboard` 3 (Windows clipboard), `clap` 4 | CLI UX + clipboard demo |

Other Rust deps across the workspace: `thiserror`, `png`, `clap`, `arboard`,
plus their transitive deps. No C library or build.rs is required.

---

## 7. `unsafe` analysis

| crate | `unsafe { }` blocks | `unsafe fn/ extern` |
|---|---|---|
| `capture-core` | 0 | 0 |
| `capture-platform-api` | 0 | 0 |
| `capture-annotation` | 0 | 0 |
| `capture-render` | 0 | 0 |
| `capture-actions` | 0 | 0 |
| `capture-windows` | 33 | 2 (`enum_monitor_cb`, `enum_window_cb`) |
| `capture-linux` | 0 | 0 |

`unsafe` is fully confined to the platform crate, as the spec requires. The
`unsafe` in `capture-windows` is the Win32 handle/DC lifetime management plus the
two `extern "system"` callbacks; there is no `unsafe` in any domain crate.

---

## 8. Tests

`cargo test --workspace` → **49 unit tests, 0 failures, 0 warnings.**

| crate | tests | covers |
|---|---|---|
| `capture-core` | 35 | geometry ops, negative coords, clamp/translate/inflate/union, coordinate mapper, pixel formats, BGRA→RGBA, snap ranking, toolbar placement (incl. bottom/top/fullscreen/tiny/negative-monitor/clamp + a sweep property test) |
| `capture-annotation` | 6 | session transitions Idle→…→Editing→Idle, default full-frame commit, snap-candidate preview+commit, pen/rect append, undo |
| `capture-render` | 5 | crop copy, negative-origin crop, deterministic pen, **golden checksum**, PNG header |
| `capture-actions` | 3 | copy PNG payload, save writes file, stable ids |

`docs/architecture/*` and `docs/adr/*` are part of the frozen tree.

---

## 9. CLI verification (real Windows run)

The `capture-cli` acceptance tool is the frozen-Core gate. All of the following
ran on this host against real monitors:

```
$ capture-cli self-test
  core api version: 0.1.0
  capture capabilities: ... multi_monitor=true per_monitor_dpi=true
  OK

$ capture-cli monitors
  [0] \\.\DISPLAY3 bounds=(-2560,0 2560x1440) scale=1.5 primary=false
  [1] \\.\DISPLAY2 bounds=(0,0 2560x1440) scale=1.0 primary=true

$ capture-cli capture-monitor 0 --output out-monitor0.png
  captured monitor 0: 2560x1440 origin=(-2560,0) format=RGBA8
$ capture-cli capture-monitor 1 --output out-monitor1.png
  captured monitor 1: 2560x1440 origin=(0,0) format=RGBA8

$ capture-cli candidates-at 100 100
  candidates-at (100,100): 5
    id=198230 kind=Window bounds=(0,0,2560x1440) label="Windows 输入体验"
    id=328992 kind=Window bounds=(0,0,2560x1439) label="...Google Chrome"
    id=658488 kind=Window bounds=(-8,-8,2576x1408) label="DeepSeek Harness - ..."
    id=0 kind=Desktop bounds=(-2560,0,5120x1440) label=None
    ...

$ capture-cli test-toolbar-placement
  [PASS] below_middle / above_bottom / inside_fullscreen / tiny_below / negative_origin_inside
  toolbar placement battery: ALL PASS

$ capture-cli render-test
  render-test: 80x48 checksum=0x40DC0DDD09DFFF4B            ← matches unit golden exactly

$ capture-cli session-test 0
  session-test: monitor=0 -> 2560x1440 annotations=1 checksum=0x2D7AA8F88787A014 capture_latency=11.7µs

$ capture-cli save 0 out.png / pin 0 out.png / ask-ai 0 out.png / copy 0 --clipboard
  save from shared SaveAction; pin/ask-ai payloads written; clipboard -> OK
```

The CLI's render `checksum` agrees with the renderer golden test (`0x40DC0DDD09DFFF4B`),
so the final-image pipeline is identical between the tests and the CLI path that
the two frontends will use.

---

## 10. Benchmark (methodology included)

- **capture-monitor (release, cold process):** ~93 ms average over 3 runs of
  `capture-cli capture-monitor 1` (2560×1440). This includes process startup
  (roughly 50–80 ms) plus GDI capture plus PNG encode (~900 KB). It is a
  whole-operation number, not a pure capture number.
- **Session T1−T0:** the in-session `Timing` measured 11.7 µs for the
  `Begin → FrameReady` application; note this only brackets command application —
  the backend capture happens *before* `FrameReady` is delivered. The true
  backend capture latency is what a frontend measures at the `capture_monitor`
  call site (see below).
- **`capture_latency = T1 − T0`** is meant to be measured around the actual
  capture, with `T0` at hotkey and `T1` when the frame is ready. The Core exposes
  `Timing { t0_hotkey_received, t1_frame_ready }` and
  `Timing::capture_latency()`.
- **Image copies (to be measured by each frontend):** Core delivers one
  `CapturedFrame` (one BGRA→RGBA copy in `capture-windows`); a frontend then
  does its own `CapturedFrame → toolkit image` conversion. Count and report per
  frontend.
- **Release binary size:** `capture-cli.exe` = **956,416 bytes (934 KiB)**.
  (A frontend app size is a per-frontend metric.)
- **Build:** `cargo build --release` finished in ~35 s after warm cache; a cold
  workspace build fetches `windows`, `png`, `clap`, `arboard` and their deps.

---

## 11. Known issues / limitations

1. **GDI + DWM/composited content:** protected or exclusive-fullscreen content
   can appear black under GDI `BitBlt`. Deferred to a DXGI/WGC route (ADR-0001).
2. **Capture is not realtime:** `capture-monitor` is a one-shot; frontends must
   not expect a high-FPS backend preview (they preview from their own surface).
3. **Snap Z-order precision:** window candidates follow OS `EnumWindows` Z-order
   on Windows; `rank_candidates` is a shared fallback and may reorder by area in
   edge cases. Element-level snapping is not implemented (`element_level=false`).
4. **Linux is a route, not a deliverable** (see §5). No real capture was run on
   this (Windows) host.
5. **Clipboard:** `capture-actions` produces payloads only; the OS clipboard
   write is the caller's job. The CLI demonstrates it via `arboard`, which works
   in a console process subject to OLE/STA behavior on the caller side.
6. **Single-frame selection model:** the session commits one crop; multi-region /
   scroll capture is out of scope for the demo.
7. **Clipboard best-effort:** the CLI `copy --clipboard` may fail on headless or
   restricted contexts; it falls back to writing the PNG payload.

---

## 12. How each UI frontend plugs into Core

1. **Create the app package** (`apps/capture-slint` or `apps/capture-qml`), add
   it to the workspace member list, and depend on `capture-core`,
   `capture-platform-api`, `capture-annotation`, `capture-render`,
   `capture-actions`.
2. **Select a platform** (one line, OS-conditional):
   ```rust
   #[cfg(windows)] let platform = capture_windows::WindowsPlatform::new();
   #[cfg(target_os = "linux")] let platform = capture_linux::LinuxPlatform::new();
   let capture: &dyn CaptureBackend = platform.capture_backend();
   let snap:     &dyn SnapBackend   = platform.snap_backend();
   ```
   Both frontends consume the **same** backend, and it is not built on
   Qt/Slint screen-grab APIs.
3. **Capture:** `capture.monitors()?` → `capture.capture_monitor(id)?` gives a
   `CapturedFrame`. Build the frozen background by adapting
   `CapturedFrame` → toolkit image/texture in the frontend (the image bridge is
   deliberately a frontend experiment variable). Then feed the frame to the
   session via `CaptureCommand::FrameReady`.
4. **Drive the session:** translate OS pointer/keyboard events into
   `CaptureCommand`s and call `session.apply(cmd)`; render from
   `session.state()` + the returned `CaptureEvent`s. `session.frame()` exposes the
   source frame, `session.hover_candidate()` the current snap candidate.
5. **Snap:** call `snap.candidates_at(point)` for highlight, and tell the session
   with `CaptureCommand::SnapCandidate(...)`. Draw the highlight/selection/handles
   yourself (frontend responsibility).
6. **Final output:** for Copy/Save/Pin/AskAI, call the action's
   `invoke(&document)` and dispatch the returned `ActionOutcome`. The PNG bytes
   are identical across both frontends because they come from the shared
   `capture-render` flattener.
7. **Pin window** creation (frameless, always-on-top, drag/close) is a frontend
   responsibility; it consumes the `ActionOutcome::Pin(payload)` bytes.
8. **Toolbar placement** must come from `capture_core::place_toolbar(...)` so both
   frontends are bit-identical.

## 13. Self-exclusion (how to pass the native window)

The overlay/toolbar window must never be a snap candidate. The frontend converts
its native window handle into an opaque token and hands it to the snap backend:

```rust
#[cfg(windows)]
let hwnd_value = window_handle as *mut core::ffi::c_void as u64;
snap.set_excluded_window(Some(capture_core::SnapExclusionToken::new(hwnd_value)));
// on cancel / teardown
snap.set_excluded_window(None);
```

On Windows the value is the `HWND`; the Core only passes the integer through as
`SnapExclusionToken(u64)` (UI-neutral). Use the same pattern for the Pin window
so a pinned image is never highlighted.

---

## 14. Completed standards (spec §23)

- [x] CLI verifiable (above)
- [x] tests pass (`cargo test --workspace`, 49 tests, 0 warnings)
- [x] frozen tag produced: `demo-core-v1`
- [x] Core API is a stable, documented contract the two UI agents start from

## 15. If the Core API blocks a frontend

Do **not** fork Core per-branch. Add a `CORE_CHANGE_REQUEST.md` (template in
`docs/CORE_CHANGE_REQUEST_TEMPLATE.md`) describing what is missing, whether both
frontends need it, why it cannot be solved in the adapter, and the minimal API
change. After the Core updates, both branches re-sync to a new shared tag.
