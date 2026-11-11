---
branch: feat/ui-rebuild
---

# UI rebuild: migration

Read when planning or implementing the item-by-item migration of the Bar, its widgets and the other windows onto the foundation from [the foundation spec](../2026-11-11-ui-rebuild/00-overview.md).

## Goal

Bring every surface back on the new foundation, one group at a time, so that `feat/ui-rebuild` can merge: the Bar shell, the eight widgets (Clock, CPU, Battery, Wi-Fi, Keep Awake, Weather, Media, Spaces), the Accessibility guide and the test window. Each item shows what it shows today and is written new on the foundation's tokens, components, queries and widget registry. The layout entries in `bar.toml` gain typed per-widget options.

## Non-goals

- Copying code. The UI on `main` (`git show main:src/ui/src/shell/...`) and `docs/features/bar.md` on `main` are a **behavior specification only**: what an item shows, its states, thresholds, labels and click commands. No file, component structure, style sheet or test is ported, and nothing comes from `_legacy/`.
- Pixel parity with `main`. The look is today's, rebuilt on tokens and checked by the user's eye and screenshot baselines. Light mode is new and uses the Latte values.
- New widgets, popovers or tooltips; changes to providers, cadences, commands, capabilities or the CSP.
- The settings UI, a command that validates a candidate configuration file, and writing configuration from the UI.
- Icons chosen from `bar.toml`. The app and workspace icon tables stay in code.
- Changing what Tiling publishes for the strip (for example adding a bundle identifier). App icons stay keyed by application name.

## Decisions

User decisions (2026-11-11):

1. **Today's look, rebuilt.** Content, states, sizes, colors and motion follow the Bar on `main`; the implementation is new.
2. **Stop after each group.** The orchestrator commits a group and stops so the user can look at it in the running app. Groups: A (parts 01–02), B (03), C (04), D (05), E (06), F (07).
3. **Options mechanism plus a few options.** Layout entries become typed per widget. Only Clock and CPU gain options now.
4. **App icons.** Hugeicons free first; `@icons-pack/react-simple-icons` only for a brand Hugeicons free does not have; a generic Hugeicons icon otherwise. The table grows to cover the apps in the user's Tiling configuration and widely used macOS apps.

Orchestrator decisions:

5. **Entry shape.** Options are flat keys of the entry table next to `widget` and `id` (`{ widget = "clock", show_seconds = false }`). The published item and the JSON Schema are unions tagged by `widget`, which is what a schema-driven settings form needs.
6. **One shared building block for Bar items.** A `BarItem` component is the rounded item every widget renders into (static or clickable). Widgets hold no layout or color logic of their own beyond their content.
7. **Commands are mutations.** Click commands go through `useMutation` factories in `queries/`; an outcome draws nothing and non-success outcomes are logged with `console.debug`, as today.
8. **Compact mode is a media query.** The "laptop" rules (`width <= 2036px`) come from one `defineConsts` media query and one `useMediaQuery` hook.
9. **Screenshot baselines need the font.** They run only when Maple Mono NF is installed and report an explicit skip otherwise; they never pass silently without it.
10. **`@icons-pack/react-simple-icons` returns** as a dependency in part 06, pinned exactly.

## Parts

| Part                           | Group | Summary                                                                                      | Status |
| ------------------------------ | ----- | -------------------------------------------------------------------------------------------- | ------ |
| [01](01-widget-options.md)     | A     | Typed per-widget options in `[layout]`, schema union, published items, `WidgetProps.options` | draft  |
| [02](02-bar-shell.md)          | A     | Bar root rules, slide-away, `BarItem`, `ScrollingLabel`, compact mode, command mutations     | draft  |
| [03](03-system-widgets.md)     | B     | Clock (with options), CPU (with option), Battery, Wi-Fi, Keep Awake                          | draft  |
| [04](04-weather.md)            | C     | Weather widget: inputs command, providers, refresh, icon tables, click                       | draft  |
| [05](05-media.md)              | D     | Media widget: label, artwork by id, retained artwork, player icons, click                    | draft  |
| [06](06-spaces.md)             | E     | Spaces widget: workspaces, apps, focus clicks, motion, extended icon tables                  | draft  |
| [07](07-windows-and-finish.md) | F     | Accessibility guide, test window, exhaustive registry, full scenario suite, final docs       | draft  |

## Affected docs

`docs/features/bar.md` (each part restores its section as shipped), `docs/architecture/ui.md` (components, widgets, mutations, options), `docs/architecture/config.md` (entry options), `docs/guides/testing.md` (screenshot baselines), `docs/index.md`, `docs/roadmap.md`, `AGENTS.md` (drop "being rebuilt").

## Live verification

Default checks use fakes and WebKit. After each group the user looks at the running app; the orchestrator asks right before any launch it makes itself. `bun run measure:bar` runs once after part 07 with the user's go-ahead; one run matching the baseline is enough.
