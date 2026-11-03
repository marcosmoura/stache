# UI and window boundary

Read when reviewing typed frontend access or on-demand windows; planned part 5 documentation.

Materialize as `docs/architecture/ui.md`; no production module surface is implemented in this spec.

## Tree and state

`src/ui/src/sdk` contains generated client/events, Mocha tokens and SDK exports. Shared visual
primitives live in shell/components and may be exported through SDK. UI layout is:

```text
src/ui/
  index.html, index.tsx, main.tsx, main.css
  tests/setup.ts                 setup/fixtures/mocks only, never suites
  src/
    sdk/                         bindings.gen.ts and typed client/foundations
    shell/
      components/ComponentName/   ComponentName.tsx, ComponentName.test.tsx,
                                 ComponentName.styles.tsx, index.ts
      modules/moduleName/        moduleName.tsx, moduleName.test.tsx,
                                 moduleName.styles.tsx, index.ts
    utils/                       utilName.ts, index.ts only when needed
```

Components/modules are siblings under shell; actual tests are colocated, including utility/client
tests. Vite root is src/ui, with required index.html; main.tsx bootstraps root from index.tsx.
Part 1 provides Shell as a component, no speculative tray/bar modules; tray remains native.
Part 5 WindowRouter/TestWindow follow the component convention. Pilot has no module UI.
Keep React/TS/Vite/Linaria, React Query for server data and zustand for local UI state. Do not carry
tauri-plugin-zustand persistence into host; config files remain authoritative for future settings.

Wyw/stylelint/lint-staged must include `.styles.tsx`, with extraction/lint/build verification in part 1.
Repository suite glob is `src/ui/src/**/*.test.{ts,tsx}`, root-relative Vitest `src/**/*.test.{ts,tsx}`;
setup is tests/setup.ts relative to frontend root. tests/ is excluded from suite/coverage source globs.

## Bindings

Use tauri-specta command/event collection with concrete Rust DTOs and explicit extra-type export.
Current documented family: Tauri 2, specta/tauri-specta rc.25, specta-typescript 0.0.12. Pin only after
headless compile/export spike against actual lockfile. Report incompatibility rather than hand mirroring.

Committed src/ui/src/sdk/bindings.gen.ts uses the host's collection. Headless --check generation
compares temp output; lint/build/CI fail on drift. Config type export is verified per serde representation;
JSON schema stays validation authority. Generic JSON routing does not infer method-specific wrappers.
No handwritten event-name shadow file. Subscriptions return cleanup and are disposed by owning UI.

## Windows

No fixed windows in tauri.conf.json, no launch-time webview. Window requests reference declared surfaces
and owner-local keys; arbitrary URLs/native flags are rejected. Host owns handles, reuse/focus/close policy
and module-stop cleanup. Only explicit Standard dev/test window works in this scope.

Launcher focus intent and nonactivating all-spaces panel intent are reserved kinds returning unsupported
errors until future module specs implement native behavior. Do not implement a bar/launcher prematurely.
Pilot requests no window. Capabilities/CSP grant only actual test-host operations.

Browser WebKit tests cover the test window/router/client with typed fakes; real Tauri smoke tests verify
main-thread creation, webview count, reuse and cleanup.
