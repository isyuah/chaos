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

`apps/capture-slint` and `apps/capture-qml` are **not** workspace members yet;
their agents add them to the `[workspace] members` list and depend on the Core
crates. If the Core API blocks a frontend, file a `CORE_CHANGE_REQUEST.md` (see
`docs/CORE_CHANGE_REQUEST_TEMPLATE.md`) instead of forking the Core.
