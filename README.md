# capture-ui-eval — shared Rust Core

This workspace is the **shared Core** for a screenshotting UI technology
evaluation: Slint vs Qt Quick/QML. Per the spec (`00_DEMO_COMMON_SPEC.md`), the
Core is implemented **once**; the two frontends (`apps/capture-slint`,
`apps/capture-qml`) are built by separate agents against the frozen Core API.

## What is here (Core session)

```text
crates/capture-core          geometry (+negative coords), capture/snap data, placement, ActionId
crates/capture-platform-api   CaptureBackend + SnapBackend traits
crates/capture-annotation     annotation document, undo, capture session state machine
crates/capture-render         flatten + PNG encode + golden tests
crates/capture-actions        Copy / Save / Pin / AskAI payloads
crates/capture-windows        GDI capture + window snap (real)
crates/capture-linux          X11/EWMH + native Wayland Portal/PipeWire capture
tools/capture-cli             acceptance CLI for the frozen Core
docs/                         architecture + ADRs + change-request template
```

## Build & test

```sh
cargo build --workspace
cargo test --workspace          # shared Core and regression tests
cargo clippy --workspace --all-targets -- -D warnings
```

## CLI (frozen-Core gate)

```sh
cargo run -p capture-cli -- --help
cargo run -p capture-cli -- monitors
cargo run -p capture-cli -- capture-monitor 0 --output out.png
cargo run -p capture-cli -- capture-virtual-desktop --output virtual.png
cargo run -p capture-cli -- candidates-at 100 100
cargo run -p capture-cli -- test-toolbar-placement
cargo run -p capture-cli -- render-test
cargo run -p capture-cli -- session-test 0
```

The monitor command prints stable IDs and ordinals. The ordinal is convenient
for the CLI; frontends should retain the stable `MonitorId`. See
`CORE_BASELINE_REPORT.md` for the historical tag, current repairs, real Windows
verification, known limitations, and frontend integration contract.

## Frontend note

`apps/capture-slint` is the working Slint frontend and is part of the workspace;
it consumes the same Core crates and platform adapters as the CLI. The QML
frontend remains a separate evaluation target and is not included yet. On a
desktop Linux session, `cargo run -p capture-slint` uses the XDG ScreenCast
portal/PipeWire path on native Wayland. By default the frontend captures the
virtual desktop; set `CAPTURE_MONITOR=N` to force one monitor for isolated
testing.
If a frontend needs a Core API change, file a `CORE_CHANGE_REQUEST.md` (see
`docs/CORE_CHANGE_REQUEST_TEMPLATE.md`) instead of forking the Core.

## Slint latency logging

`capture-slint` records timestamped latency events to stderr. Set
`CAPTURE_SLINT_LOG` to also append the same events to a file. The log includes
backend startup, monitor enumeration, capture and RGBA conversion, window
creation, first pointer-down handling, snap queries, sampled pointer input, and
visual refresh durations.

PowerShell example:

```powershell
$env:CAPTURE_SLINT_LOG = (Join-Path $pwd "capture-slint.log")
$env:SLINT_BACKEND = "skia"
cargo run -p capture-slint
```

For a smaller startup sample, set `$env:CAPTURE_MONITOR = "0"`; remove that
variable to measure the default virtual-desktop capture. On Linux, use the
equivalent `CAPTURE_SLINT_LOG=/tmp/capture-slint.log SLINT_BACKEND=skia`
environment variables. Reproduce one selection and one pen stroke, then
inspect the `startup.*`, `input.down.*`, `snap.*`, `visual.*`, and
`render.*` entries.
