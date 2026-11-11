---
status: draft
---

# 01 — Scaffold

## Scope

Delete the current UI and its tooling, then create the new project skeleton in `src/ui/`: toolchain, entry files, window router, placeholder window, app providers shell, the Vitest harness and the Playwright end-to-end harness. No tokens, no data layer and no Bar yet.

Nothing is copied from the deleted tree or from `_legacy/`. Root configuration files stay at the repository root.

## Requirements

### Removal

- Delete everything tracked under `src/ui/`, including the generated bindings, which are regenerated at their new path.
- Remove the Linaria/wyw toolchain, stylelint and its configs (`stylelint.config.mjs`, `.stylelintignore`), `@icons-pack/react-simple-icons`, `postcss*`, `@styled/typescript-styled-plugin`, the Bar parity gate (`src/ui/tests/bar-parity`, `bar-reference`, `scripts/bar-reference`, the `test:parity` script, the parity Vitest projects and browser commands) and every reference to them in `package.json`, `vite.config.ts`, tsconfig files, `.oxlintrc.json`, `.oxfmtrc.json`, `lint-staged.config.ts`, `.github/` and scripts.
- Keep `scripts/measure-bar*` and `scripts/measure-foundation.ts`. If one reads a deleted UI path, fix the path only.

### Project

- Dependencies as in overview decision 14, exact pins, installed with Bun. `bun.lock` is updated.
- `vite.config.ts`: `stylex.vite({ useCSSLayers: true, ... })` first, then `@vitejs/plugin-react`, then the React Compiler Babel preset. Configure StyleX module resolution and aliases so `defineVars` imports resolve the same in dev, build and tests. Dev wires the StyleX virtual stylesheet and runtime as the StyleX Vite guide describes. Build targets stay `safari26`.
- One path alias, `@/` → `src/ui/src/`, defined once and shared by Vite, TypeScript and StyleX.
- `src/ui/index.html` loads `/src/main.tsx`. `main.tsx` only imports `main.css`, creates the root in `StrictMode` and renders `App`.
- `main.css` declares the cascade layer order (reset below the StyleX layers), a minimal reset and transparent `html`/`body`/`#root`. It holds no component styles.
- `src/app/App.tsx` composes providers (an empty composition point in this part plus a top-level error boundary) around the routed window inside `Suspense`.
- `src/router.ts` exports the surface table and a resolver. Surfaces are `shell` (default when `surface` is absent), `test`, `access` and `bar`; each maps to a `React.lazy` window so every window is its own chunk. In this part all four map to `windows/Placeholder`. A missing, empty or unknown value resolves to a local fallback that calls no native command. `router.ts` contains no JSX.
- `windows/Placeholder/` follows the directory convention (`Placeholder.tsx`, `Placeholder.style.ts`, `Placeholder.test.tsx`, `index.ts`) and shows the surface name. Its style file uses `stylex.create`, which proves the StyleX pipeline end to end.
- The binding generator writes `src/ui/src/ipc/bindings.gen.ts`. Update the generator, its Rust tests and every ignore list to the new path. The file stays excluded from lint and format.
- Lint: oxlint keeps the existing rule set, adds `@stylexjs/eslint-plugin` through `jsPlugins` (`valid-styles`, `no-unused`, `valid-shorthands`, `enforce-extension`, `sort-keys` off) and restricts imports of `@tauri-apps/*` and `**/bindings.gen` to files under `src/ui/src/ipc/` and `src/ui/tests/`/`src/ui/e2e/`. If oxlint cannot load the StyleX plugin, stop and report; do not silently drop it.
- Scripts: `lint:ui` loses stylelint; `test` becomes `test:ui`, `test:e2e`, `test:scripts`, `test:native`; `test:e2e` runs Playwright.

### Harnesses

- **Vitest** (`src/ui/tests/`): browser mode, WebKit, `setup.ts`, a `render` helper that wraps a component in the app providers, and coverage thresholds at the current values (80/80/65/80). Tests are colocated as `*.test.ts(x)`.
- **Playwright** (`src/ui/e2e/`): `playwright.config.ts` at the repository root or in `e2e/`, WebKit only, `webServer` starting the Vite dev server on a port that is **not** 1420 so it cannot collide with a running `tauri:dev`. A fake-host fixture installs the Tauri IPC internals with an init script before the page loads, answers commands from a per-test script, lets a test emit events, and records every command call. Scenario files live in `src/ui/e2e/scenarios/*.spec.ts`. Test output goes to an ignored directory.

## Acceptance criteria

- [ ] UI-01-01: No file from the old `src/ui/` remains; `git grep` finds no reference to Linaria, wyw, stylelint, simple-icons or the parity gate outside `_legacy/` and `docs/specs/`.
- [ ] UI-01-02: `bun run build` succeeds and emits one CSS asset that contains the Placeholder's StyleX rules inside cascade layers, and a separate JS chunk for the Placeholder window.
- [ ] UI-01-03: `?surface=shell|test|access|bar` and no `surface` render the placeholder with the surface name; an unknown or empty value renders the fallback and makes zero IPC calls.
- [ ] UI-01-04: `bun run check:generated` passes with the bindings at `src/ui/src/ipc/bindings.gen.ts`.
- [ ] UI-01-05: An import of `@tauri-apps/api` outside `src/ui/src/ipc/` fails `bun run lint`; an invalid StyleX property fails `bun run lint`.
- [ ] UI-01-06: `bun run lint`, `bun run format:check` and `bun run test` pass, and `bun run test` runs the Playwright scenarios.
- [ ] UI-01-07: `docs/architecture/ui.md` describes the new layout, conventions and harnesses; `AGENTS.md`, `docs/guides/testing.md`, `docs/features/bar.md` and `docs/index.md` no longer describe the deleted UI as shipped on this branch.

## Tests

- `router.test.ts`: every known surface, absent, empty, unknown, repeated parameter, case sensitivity.
- `Placeholder.test.tsx`: renders the surface name; computed style proves a StyleX rule applied.
- `App.test.tsx`: a window that throws renders the error boundary fallback.
- End-to-end `routing.spec.ts`: the production `index.html` with the fake host for each surface, the unknown-surface fallback with zero recorded commands, and no console errors.
- Rust: the generator and bindings tests pass at the new path.

## Notes

- Read the StyleX Vite installation guide and the unplugin configuration page before configuring; the dev virtual modules and module-resolution options change between releases.
- `.claude/` inside the spec directory is a stray folder from an earlier session and is not part of the spec.
- Never launch the desktop app in this part.
