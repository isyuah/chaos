# CORE_CHANGE_REQUEST

**Author:** (UI agent, e.g. `capture-slint` / `capture-qml`)
**Date:** 
**Applies to frozen Core tag:** `demo-core-v1`

Use this only when the frozen Core API genuinely blocks a reasonable frontend
implementation. Do **not** fork the Core in your own branch. Before writing,
re-read `docs/architecture/module-boundaries.md` and the Core API section of
`CORE_BASELINE_REPORT.md`.

## 1. What is missing

_Describe the concrete capability the Core does not expose, with a minimal repro or
a short trace._

## 2. Is it needed by both frontends?

- [ ] `capture-slint`
- [ ] `capture-qml`

If only one frontend needs it, it is likely a frontend/adapter concern, not a Core
one — explain why here.

## 3. Why can't it be solved in the adapter?

_Show the boundary where the adapter cannot bridge (e.g. the Core does not expose
a value the frontend cannot derive, or the Core forces a toolkit-specific type)._

## 4. Suggested minimal API change

_Draft the smallest change (new type, extra method on a trait, or a new event),
staying UI-neutral and platform-neutral. No toolkit types, no `HWND`/X11 leaks._

## 5. Backwards compatibility

_Is it additive (existing frontends unaffected) or breaking? If breaking, propose a
coexisting path so both branches can re-sync on one new tag._
