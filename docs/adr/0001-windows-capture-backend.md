# ADR-0001: Windows capture backend for the demo

- Status: Accepted (for `demo-core-v1`)
- Date: (frozen with tag `demo-core-v1`)

## Context

The Core must expose a real `CaptureBackend` on Windows. Two UI frontends will
consume it. The demo's goal is to compare UI toolkits, not to ship a
production-grade capture stack, so the capture implementation must be *real*
(deliver physical-pixel frame data) while keeping a clean abstraction and
leaving a documented migration path.

## Decision

Use **GDI via `BitBlt`** from `GetDC(NULL)` over the whole virtual desktop,
then crop the requested monitor region. Convert the resulting BGRA buffer to
RGBA8 for `CapturedFrame`.

Rationale:

- Reliable on every Windows version and every DPI setting (with Per-Monitor-V2
  awareness). No D3D device, no message loop, no WinRT activation needed for a
  CLI or a headless test.
- Avoids the Windows.Graphics.Capture requirement that the app run with an
  `IDirect3DDevice` and a captured monitor item, which is overkill for this demo
  and harder to reason about on a background thread.
- DXGI Desktop Duplication is the production-grade successor but adds significant
  complexity (device acquisition, acquire/release frames, reprocessing, and
  fallback for protected content). It is deferred; see "Consequences".

## Approach chosen

```
        virtual desktop
   GetDC(NULL) ──CreateCompatibleDC + CompatibleBitmap──► memDC
        │                                                     │
        └─ BitBlt(memDC, 0,0, VW,VH, screenDC, VX,VY, SRCCOPY) ──►│
                                                              ▼
                                    GetDIBits → top-down BGRA rows
                                                              ▼
                                 BGRA→RGBA → full frame → crop → CapturedFrame
```

`VX,VY,VW,VH` are the virtual desktop's physical bounds, including negative
origins. Capturing the full source with `VX,VY` is important: using `(0,0)`
silently reads the primary monitor when the virtual desktop starts at a
negative coordinate. The requested monitor is cropped afterward using its
physical bounds.

## Consequences

- Simplicity and testability win for the demo.
- Full-virtual-desktop `BitBlt` per capture is not the fastest path for high-FPS
  capture; the Core deliberately does not promise realtime preview from the
  backend (frontends preview from their own surface).
- Windows.Graphics.Capture / DXGI migration is isolated inside
  `capture-windows` behind the same `CaptureBackend` trait, so it is a drop-in
  change later.
- Known limitation (recorded in CORE_BASELINE_REPORT.md): protected/DWM-composited
  content may appear black under GDI on some setups.
