---
status: done
---

# 05 — Host integration and UI SDK

Read when connecting SDK ownership to Tauri and an on-demand test window.

## Scope and files

Create src/crates/app/src/{host,tray,permissions,shutdown,windows,bindings}.rs,
src/crates/sdk/src/windows.rs, a headless Rust generate-bindings entry point,
`src/ui/src/sdk/bindings.gen.ts`, SDK client/tokens/exports beside it, and
`src/ui/src/shell/components/{WindowRouter,TestWindow}/`. Each component directory contains
`ComponentName.tsx`, `ComponentName.test.tsx`, `ComponentName.styles.tsx`, `index.ts`, substituting
the directory's name. Optional surfaces use sibling `src/ui/src/shell/modules/moduleName/` with
`moduleName.tsx`, `moduleName.test.tsx`, `moduleName.styles.tsx`, `index.ts`; pilot has none.
Update src/crates/app/tauri.conf.json/capabilities and `src/ui/{index.tsx,main.tsx,main.css}`;
retain Vite's `src/ui/index.html`. Utilities go under `src/ui/src/utils/` only when needed.

## Key APIs

Host::new(AppPaths, ConfigCatalog, ModuleFactories) owns registry/config/watchers/socket/permissions/
shutdown. PermissionManager::refresh() uses fresh trust checks. Context WindowService::request(owner,
WindowRequest) returns an owned handle. Request declares owner-local key, allowed surface/route, size
and kind (Standard, reserved Launcher or NonActivatingPanel intent). Implement Standard/test only;
future kinds return typed unsupported errors until their tool spec defines native semantics. Reject
arbitrary JS URLs/native flags; surfaces are module-declared, not global hardcoded windows.

## Requirements

- Bootstrap logging/paths and pure catalogs; create tray on main thread before slow services/prompts;
  initialize config/registry/IPC and enabled modules per thread contracts. No Dock icon. CLI bypasses host.
- Tray has config-backed Enable (atomic enabled edit preserving unrelated JSONC comments/values) and
  separate session-only Pause/Resume. Invalid/conflicting disk file is never overwritten; show diagnostic.
  Watcher reconciles accepted edits once. Disabled cannot resume; waiting/unavailable/failed show reason
  and eligible retry/request actions. Display config diagnostics separately from operational status.
  Callbacks do no blocking native work on main thread.
- Accessibility: non-prompting poll at 1 second while waiting, backoff when no modules need it; refresh
  on activation/explicit retry and periodically check grant revocation. Prompt only enabled declared
  requirements, once per session or explicit request. Grant starts waiting modules without restart.
  Pilot never requests Accessibility.
- Single instance uses next identity; second launch creates no extra registry/listeners. Quit/signals/
  Tauri exit share idempotent coordinator: stop accepting IPC/config work, cancel monitors/modules,
  stop in reverse order, join off-main and release windows/tray/socket. No main-thread cleanup deadlock.
  Preserve exit precedence; reload never restarts. Execute execOnStartup once after first host setup,
  not CLI/second-launch/reload, preserving legacy shell semantics without hotkey-crate dependency.
- No fixed windows. Explicit dev/test action creates Standard test window; launch never does. Owner/key
  requests reuse/focus per policy; close removes registry state, module stop closes owned windows.
  Router validates surfaces/cleans subscriptions. No settings/widgets/pilot UI.
- First compile/export compatibility spike: documented Tauri 2 family uses specta/tauri-specta
  =2.0.0-rc.25, specta-typescript 0.0.12. Pin verified mutually compatible versions or report blocker.
  collect_commands!/collect_events! and explicit .typ::<T>() are documented. Generic JSON dispatch
  cannot infer typed module-method wrappers; real host/test facades use concrete Rust DTOs.
- Headlessly generate committed src/ui/src/sdk/bindings.gen.ts; host mounts the same command/event collection.
  Export global/pilot config TS types where serde/derive support is verified; schema remains validation
  authority. Report unsupported representations rather than hand-mirroring. Add generate-bindings and
  --check scripts (temp export/compare, no source modification), invoked by lint/build/CI alongside
  schema checks. No desktop launch needed to generate. No mirrored event-name file.
- UI SDK wraps generated invoke/events, initial Catppuccin Mocha tokens from legacy design-system,
  only primitives TestWindow needs. React Query for server data, zustand for local state; omit
  tauri-plugin-zustand. Least-privilege capabilities and CSP, no inherited weather/media permissions.
  Visual primitives follow the shell/components convention and may be exported through SDK;
  no flat TestWindow/router TSX files or speculative tray/bar React modules. Keep `.styles.tsx`
  supported by wyw/stylelint. Tests are colocated under `src/ui/src/**/*.test.{ts,tsx}`; frontend
  `tests/setup.ts` and fixtures/mocks are support only, never suite locations.

## Acceptance criteria

- [x] H01: Host owns/wires SDK without globals; tray precedes slow startup, no Dock/webview at launch.
- [x] H02: Enable edits safely, pause stays session-only, states/diagnostics reflect reload/toggle races.
- [x] H03: Fake grant/revoke and manual macOS trust check recover without restart/repeat prompt loop.
- [x] H04: Single-instance/all exit paths clean once without deadlock; startup commands run once with
      preserved shell semantics.
- [x] H05: Explicit test-window create/reuse/close/owner cleanup works; reserved kinds fail clearly and
      no module request means no webview.
- [x] H06: Pinned compatible tauri-specta exports commands/events/supported config types headlessly;
      stale bindings/schemas fail lint/build/CI, with no hand event mirrors.
- [x] H07: WebKit tests cover generated client, subscription cleanup/router and token-based test UI;
      no competing UI config persistence.
- [x] H08: Host status exposes diagnostics, test surface is capability-restricted and new/legacy
      instance/runtime/cache state is isolated.
- [x] H09: Status reads (`snapshot`/`snapshots`/`service_ready`) never wait on module code or worker
      joins (for example a per-slot snapshot cell updated at each transition), so the tray never
      blocks the main thread while a module stops. Deferred from the part 01–03 review.
- [x] H10: A real-app smoke test runs `stache-macos` `run_on_main` jobs on the AppKit main thread,
      finishing S04's main-thread dispatch evidence. Deferred from the part 01–03 review.

## Tests and validation

Inject fake native/permission/module services for startup/races; real Tauri smoke tests for tray/main
thread, zero/on-demand webviews, second instance/signals. No CI trust prompts. Run
`bun run generate-bindings`, `bun run generate-bindings -- --check`, `bun run lint`, `bun run test`,
`bun run format:check`, `bun run build`, `bun run tauri:build`; record manual smoke evidence.

### Verified 2026-11-03

- Parent ran lint, formatting, tests (379 native, 24 WebKit), UI coverage, generated-artifact
  checks, Rustdoc with warnings denied, frontend build and release bundle/signature verification.
  Bundle contains only `stache`; generators and smoke harness are feature-gated developer tools.
- Parent repeated window/registry/permission race selections 20 times without failures.
- Release smoke with isolated HOME/config/runtime proved zero launch webviews, real AppKit dispatch,
  activation observer delivery/removal, embedded React rendering/generated invoke, IPC reload and
  status-event delivery, window reuse/close, second-instance rejection and normal SIGTERM teardown.
  A deliberately overlong socket path produced exit 1, not a false-success smoke result.
- Real Accessibility prompting and tray clicks remain manual verification gaps. Non-prompting trust
  checks, fake grant/revoke, tray rows/actions and shared exit coordination are test-covered. GUI
  automation could not reliably target the new tray alongside the legacy process; no tray-click
  success is claimed. Remote CI and notarization were not run.
- JSONC edits check conflicts immediately before atomic replacement; this is not an atomic
  compare-and-swap against a non-cooperating external writer. Concurrent creation is no-clobber.

## Docs

Write docs/architecture/ui.md; update SDK/overview/config/IPC/testing/new-module/index. Include binding
version evidence/drift commands, unsupported window kinds, permission and shutdown ownership policies.
