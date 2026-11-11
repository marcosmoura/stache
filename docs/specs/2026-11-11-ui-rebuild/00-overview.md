---
branch: feat/ui-rebuild
---

# UI rebuild: foundation

Read when planning or implementing the from-scratch React UI foundation: the toolchain, the design tokens and light/dark themes, the data layer, and the `bar.toml`-driven Bar layout mechanism.

## Goal

Replace the React UI in `src/ui/` with a new project written from scratch on StyleX, TanStack Query, zustand, Hugeicons (free), Motion for React and the typed Tauri bindings. This spec covers **only the foundation**: an empty, modern project that holds the mechanisms and no functional Bar.

At the end of this spec:

- The Bar window renders whatever `bar.toml` `[layout]` says, through a typed widget registry. No widget is registered yet, so the Bar is empty.
- Light and dark themes work through tokens, follow the system, and can be forced from `global.toml`.
- The data layer, state conventions, motion setup, icon wrapper, unit harness and end-to-end harness exist and are tested.

The item-by-item migration (Bar shell behavior, Clock, CPU, Battery, Wi-Fi, Keep Awake, Weather, Media, Spaces, Accessibility guide, test window) is a **second spec on the same branch**, written after the user reviews this foundation. Nothing merges to `main` before that migration is done.

## Non-goals

- Reusing anything from `_legacy/` or from the current `src/ui/`. The current UI on `main` may be read only to learn a host contract (a command name, an event payload, the `?surface=` values). No file, component, style, test or folder structure is copied.
- Any Bar widget, the Bar's slide-away behavior, the Accessibility guide and the test window. Their surfaces render the placeholder window until the migration spec.
- The settings UI and any command that writes configuration. The architecture must make them straightforward later (see decision 4).
- Per-widget options in `bar.toml`. The entry shape leaves room for them; each migrated widget adds its own.
- Changes to data providers, their cadences, commands, capabilities or the CSP.
- User-defined themes or token overrides from configuration. The token structure must allow them later.

## Decisions

User decisions (2026-11-11):

1. This spec replaces an earlier unapproved draft in this directory (SF Symbols, headless components, Catppuccin and Liquid Glass theme packages). None of its decisions carry over.
2. Scope is the foundation only; the user reviews it before the migration spec is written. Both specs build on `feat/ui-rebuild`.
3. The new project replaces `src/ui/` in place. The first part deletes the old UI and its tooling.
4. The Bar is `bar.toml`-driven. Every arrangement the Bar can show comes from typed, validated configuration published by the host, so a later settings UI only has to write the file.
5. Tokens start from Catppuccin: Mocha for dark, Latte for light. Appearance follows the system and `global.toml` can force it.
6. Folder convention (fixed by the user): `e2e/`, `tests/`, `index.html` and `src/` with `components/<Name>/`, `windows/<Name>/`, `router.ts`, `main.tsx`, `main.css`. Each component or window directory holds `<Name>.tsx`, `<Name>.style.ts`, `<Name>.test.tsx` and `index.ts`.
7. Icons are Hugeicons free at the latest published versions (`@hugeicons/react` 1.1.10, `@hugeicons/core-free-icons` 4.4.0 on 2026-11-11).

Orchestrator decisions:

8. **Configuration stays TOML.** The request mentions JSON files; the shipped format is TOML with generated JSON Schemas. The schema is what a settings UI will read to build its forms.
9. **Light/dark is CSS only.** Color tokens are `light-dark(<latte>, <mocha>)` values and the root sets `color-scheme: light dark`. The host sets each window's native appearance from `[ui] appearance`, so `prefers-color-scheme` in the webview is already correct at first paint. The UI runs no theme JavaScript and cannot flash the wrong theme.
10. **Layout travels in `BarView`.** The normalized layout is a field of the existing Bar snapshot and arrives through the existing ordered `bar-changed` feed. There is no new command or event.
11. **Layout entries are typed, not free-form JSON.** A zone is an ordered list; an entry is a widget name or a table with `widget` and an optional `id`. The host validates and normalizes, and the UI receives `{ id, widget }` items.
12. **React Compiler stays.** It is already part of the project and removes manual memoization. Hand-written `useMemo`/`useCallback`/`memo` are not used unless a measured case needs them.
13. **Two test levels.** Vitest browser mode in WebKit for colocated unit and component tests; Playwright Test in WebKit for end-to-end scenarios against the Vite dev server with a scripted fake host. Both run in `bun run test`.
14. **Dependency updates** in part 01: `motion` 13.1.0 → 14.12.0, `zustand` 5.0.16 → 5.1.0, `@tanstack/react-query` and its lint plugin 5.105.3 → 5.107.1, new `@stylexjs/*` 0.19.1 and `@playwright/test` matching the pinned `playwright`. All versions stay exact pins. The worker reads the Motion 14 changelog before using its API.
15. **No zustand store ships without a consumer.** The foundation ships the scoped-store helper and its conventions, proven by tests. The first real stores arrive with the widgets that need them.
16. **The branch is not shippable between parts.** Part 01 removes every surface. That is acceptable because nothing merges until the migration spec is done. Each part still commits with all checks green.

## Research basis

Checked against current documentation on 2026-11-11.

- **StyleX 0.19.** `@stylexjs/unplugin` (`stylex.vite({ useCSSLayers: true })`) goes before `@vitejs/plugin-react`. A CSS entry file must exist so the build has an asset to append to. `defineVars` and `defineConsts` live only in `*.stylex.ts` files as named exports. `defineConsts` is for values that never change at runtime (media queries, z-indices, durations). `light-dark()` in `defineVars` is the documented light/dark recipe; `createTheme` overrides a var group for a subtree. Components accept `style?: StyleXStyles` and merge it last in `stylex.props`, optionally restricted to a property set. Dynamic values use function styles.
- **TanStack Query 5.** `queryOptions` factories are the single definition of a key and its fetcher and give typed `setQueryData`. Pushed state is written with `setQueryData` and needs `staleTime: Infinity`; data that cannot change while the app runs uses `staleTime: 'static'`. `select` and tracked properties limit re-renders. IPC is not a network, so `networkMode: 'always'` and no focus or reconnect refetch.
- **zustand 5.** Client-only UI state. Vanilla `createStore` in a React context for scoped stores, atomic selectors, `useShallow` when a selector builds an object (a fresh reference loops in v5), actions grouped apart from state.
- **Motion 14.** `LazyMotion` with `strict` and the `m` components from `motion/react-m` keeps the initial cost near 5 kB; the feature bundle loads asynchronously. `domMax` is needed for layout animations. `MotionConfig reducedMotion="user"`. Motion values update without React renders. `layoutDependency` limits layout measurement.
- **Hugeicons.** One `HugeiconsIcon` renderer plus per-icon named imports that tree-shake. An app-level `Icon` wrapper owns the defaults.
- **Tauri 2.** Generated `tauri-specta` bindings are the only command and event surface. Listeners must be unlistened, including when `listen` resolves after cleanup. `@tauri-apps/api/mocks` (`mockIPC` with `shouldMockEvents`, `mockWindows`, `clearMocks`) fakes the boundary in tests.

## Target layout

```text
src/ui/
  index.html
  e2e/                        Playwright setup and utilities: fake host, harness page, scenarios
  tests/                      Vitest setup and utilities: render helpers, IPC mock, fixtures
  src/
    main.tsx                  bootstrap only
    main.css                  CSS entry: layer order, reset, root color-scheme
    router.ts                 ?surface= -> lazily loaded window
    app/                      App, providers (query client, motion, error boundary)
    ipc/                      the only code that imports @tauri-apps/* or the bindings
      bindings.gen.ts         generated
    queries/                  queryOptions factories and feeds, one file per domain
    stores/                   zustand helper and app-level stores
    theme/                    *.stylex.ts tokens, motion values
    widgets/                  widget contract and registry (one folder per widget later)
    components/<Name>/        shared components
    windows/<Name>/           one directory per window
    hooks/  utils/
```

Dependencies point one way: `windows` → `widgets` → `components` → `theme`; `queries` → `ipc`. Lint rules enforce the `ipc` boundary.

## Parts

| Part                      | Summary                                                                                           | Status |
| ------------------------- | ------------------------------------------------------------------------------------------------- | ------ |
| [01](01-scaffold.md)      | Delete the old UI and tooling; new project, toolchain, router, placeholder window, both harnesses | draft  |
| [02](02-design-system.md) | Tokens, light/dark, `[ui] appearance` applied natively, Motion setup, `Icon` component            | draft  |
| [03](03-data-layer.md)    | IPC boundary, query client, ordered feeds, host and Bar queries, zustand scoped-store helper      | draft  |
| [04](04-bar-layout.md)    | `[layout]` in `bar.toml`, `BarView.layout`, widget registry, layout engine, Bar window, scenarios | draft  |

## Affected docs

`AGENTS.md` (UI stack and test lines), `docs/architecture/ui.md` (rewritten in part 01, extended by each part), `docs/architecture/config.md` (`[ui]`, `[layout]`), `docs/features/bar.md` (marked as being rebuilt, layout section), `docs/guides/testing.md` (parity gate removed, end-to-end harness), `docs/index.md`, `docs/roadmap.md`.

## Live verification

Default checks use fakes and WebKit only. A native run (appearance following the system and `global.toml`, layout reload) needs the user's go-ahead right before each launch. Without it, those items are recorded as unverified.
