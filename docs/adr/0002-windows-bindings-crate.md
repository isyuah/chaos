# ADR-0002: Windows FFI bindings crate

- Status: Accepted (for `demo-core-v1`)
- Date: (frozen with tag `demo-core-v1`)

## Context

`capture-windows` needs Win32 APIs for GDI capture and window enumeration. We
must not hand-write raw `extern "system"` declarations nor leak the bindings
crate's types outside `capture-windows`.

## Decision

Use the official Microsoft **`windows`** crate (v0.61), restricted to the exact
feature set needed:

```text
Win32_Foundation
Win32_Graphics_Gdi
Win32_UI_HiDpi
Win32_UI_WindowsAndMessaging
Win32_System_Com
Win32_System_Threading
Win32_Graphics_Dwm
```

Rationale:

- First-party, generated, type-safe, and toolchain-maintained.
- Feature-gated so the dependency tree stays small and no linker bloat from
  unused WinRT namespaces.
- The same crate family is widely used, so future DXGI / WGC integration
  (ADR-0001 consequences) reuses the same dependency.

## Consequences

- All Win32 types (`HWND`, `HDC`, `HBITMAP`, `MONITORINFO`,
  `DPI_AWARENESS_CONTEXT`) stay inside `capture-windows`; the public API only
  exposes `u64`/`PhysicalPoint` from `capture-core`.
- Feature additions (e.g. `Win32_Graphics_Dxgi` for WGC) are a Cargo.toml change
  plus new code, all contained behind the same trait.
- Note: raw `HWND` values are converted to `SnapExclusionToken(u64)`; that is an
  opaque identity, not a leaked dependency.
