---
status: done
---

# 07 — Other windows and finish

## Scope

The Accessibility guide and the test window as real windows, the exhaustive registry, the full scenario suite and the final documentation. Depends on parts 01–06.

## Requirements

### Accessibility guide (`windows/AccessibilityGuide/`)

- Reads `appInfo` and the host status through the foundation's queries and feed.
- States: checking (a loading line); blocked (guidance with numbered steps, an identity card with name, bundle identifier and app path, a collapsed troubleshooting section with a variant for an unbundled binary); revoked (alternate title and line); open ("Access granted. Starting Stache Next…", then the window closes itself).
- Actions: request access, open Accessibility settings, quit, each a mutation that disables its button while pending. `HostStatus.error` is shown when present.
- Chrome: frameless transparent window with one rounded panel, a drag region in the header with the Close button outside it, a scrolling body and a fixed action footer. Escape and Close close the window; key auto-repeat is ignored.
- The look follows today's guide on tokens, in both themes, with the tone of the hero badge following the state.

### Test window (`windows/TestWindow/`)

- Read-only status surface: loading, unavailable, host-ready and empty module and config states. Module runtime detail and rejected-configuration diagnostics have separate labels, as do file diagnostics and warnings. `revision` is rendered as text and never converted to a number.

### Router and registry

- `router.ts` maps `access` and `test` to these windows; `shell` keeps the placeholder as the development fallback.
- The production widget registry becomes an exhaustive `Record<BarWidgetKind, …>`, so a kind added in Rust fails type checking until it is registered.
- Any IPC call these windows need goes through `ipc/` and `queries/`; window-level Tauri APIs (close, drag) get a small wrapper in `ipc/`.

### Finish

- A full end-to-end session for the Bar with the production registry and the default layout: boot, all widgets populated, a layout reload that reorders and removes widgets, a theme switch, the menu bar appearing and disappearing, and Tiling and media activity interleaved, with no console error.
- `docs/features/bar.md`, `docs/architecture/ui.md`, `docs/guides/testing.md`, `docs/index.md`, `docs/roadmap.md` and `AGENTS.md` describe the UI as shipped, with no "being rebuilt" wording.
- `bun run test:ui:coverage` meets the thresholds.

## Acceptance criteria

- [x] MIG-07-01: The guide renders every state, including the unbundled variant and the error line, and closes itself on `open`.
- [x] MIG-07-02: Each guide action sends its command once, disables while pending, and Escape and Close close the window.
- [x] MIG-07-03: The test window renders every state with the distinct labels and never parses `revision`.
- [x] MIG-07-04: `?surface=access` and `?surface=test` render the new windows and make only the calls their capabilities allow.
- [x] MIG-07-05: The registry is exhaustive at the type level.
- [x] MIG-07-06: The full Bar session passes with no console error and no unanswered command.
- [x] MIG-07-07: Docs describe the shipped UI; no document says the UI is being rebuilt.
- [x] MIG-07-08: `bun run lint`, `bun run format:check`, `bun run test`, `bun run build` and `bun run test:ui:coverage` pass.

## Tests

- `AccessibilityGuide.test.tsx` and `TestWindow.test.tsx` for every state and action.
- End-to-end `access.spec.ts` (blocked, request, revoked, open and self-close) and `test-window.spec.ts`.
- End-to-end `bar-session.spec.ts` as described under Finish.
- Screenshot baselines for the guide (blocked, revoked) and the test window in dark and light.

## Notes

- Behavior reference on `main`: `docs/architecture/ui.md` (Accessibility guidance window, Test window) and the two window directories.
- Native checks for the user after this part: the Accessibility flow on a fresh grant, the test window from the tray, and `bun run measure:bar`.
