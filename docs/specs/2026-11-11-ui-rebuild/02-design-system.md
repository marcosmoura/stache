---
status: done
---

# 02 — Design system

## Scope

The token system with light and dark values, the appearance setting applied natively by the host, the Motion setup and the `Icon` component. Depends on part 01.

## Requirements

### Tokens (`src/ui/src/theme/`)

- Token files are `*.stylex.ts` and export only `defineVars`/`defineConsts` results.
- Two tiers:
  - **Palette** (`palette.stylex.ts`): the raw Catppuccin Latte and Mocha colors as `defineConsts`. Only token files import it; a lint rule or test forbids importing it from components.
  - **Semantic** (`tokens.stylex.ts`, split into more files if it grows): `defineVars` groups that components use. Color values are `light-dark(<latte>, <mocha>)`. Groups: `color` (surfaces, text levels, border, accent, and status tones for success, warning, danger and info), `space`, `radius`, `font` (family, sizes, weights, line heights), `shadow`, and `size` (control and icon sizes).
- Static values that never theme are `defineConsts`: `duration`, `easing`, `zIndex` and media queries (reduced motion, reduced transparency, more contrast).
- The font family token keeps today's stack, `'Maple Mono NF', 'SF Mono', monospace`; fonts are system installed and never bundled.
- `main.css` sets `color-scheme: light dark` on `:root`. No JavaScript reads or sets the theme.
- The semantic groups are the extension point for later custom themes: a `createTheme` over a group, or a function style that sets one var, overrides them for a subtree without touching components. Ship one small test-only `createTheme` that proves a subtree override works; ship no production theme switcher.
- Document the rules in `docs/architecture/ui.md`: components use semantic tokens only, no raw color or pixel literals where a token exists, a `style?: StyleXStyles` prop merged last for overrides, conditional styles through `stylex.props` arguments, and function styles for runtime values.

### Appearance (Rust)

- `GlobalConfig` gains `[ui] appearance = "system" | "light" | "dark"`, default `"system"`, snake_case, with schema, template comment, generated bindings and the catalog spelling test. An unknown value is a type error naming `ui.appearance`.
- The host applies it natively and stops forcing dark:
  - `Standard` windows (`src/crates/app/src/windows.rs`): theme `None`, `Light` or `Dark` at build.
  - The Bar panel (`src/crates/macos/src/panel.rs`): inherit, Aqua or Dark Aqua.
  - An accepted `global.toml` reload re-applies the value to every live window and panel on the main thread, without recreating them. A rejected reload changes nothing.
- The appearance is a pure function from the config value to the native value, unit tested without AppKit.

### Motion (`src/ui/src/app/` and `src/ui/src/theme/`)

- `MotionProvider`: `LazyMotion` with `strict` and an asynchronously imported `domMax` feature module, wrapping `MotionConfig` with `reducedMotion="user"` and the default transition from the motion values. It joins the `App` provider composition.
- Components import `* as m from 'motion/react-m'`. A lint restriction forbids importing `motion` (the full component) from `motion/react`.
- `theme/motion.ts` holds durations (seconds) and easings as numbers for Motion, plus named transition presets. The `duration`/`easing` consts in the StyleX file hold the same values as CSS strings; a test asserts the two agree.
- Tests can disable animation globally through the Vitest setup so assertions do not wait on timing.

### Icon (`src/ui/src/components/Icon/`)

- `Icon` wraps `HugeiconsIcon`: required `icon`, optional `size` (default from the `size` tokens), `strokeWidth` default 1.5, `color` fixed to `currentColor`, optional `label`. Without `label` the icon is `aria-hidden`; with it the icon has `role="img"` and that accessible name. It accepts the restricted `style` prop.
- Icons are imported by name at the call site from `@hugeicons/core-free-icons`. No file re-exports the icon set.

## Acceptance criteria

- [x] UI-02-01: A component styled with semantic color tokens computes Latte values under `prefers-color-scheme: light` and Mocha values under `dark`, with no script involved.
- [x] UI-02-02: A test-only `createTheme` overrides a token for its subtree and leaves siblings unchanged.
- [x] UI-02-03: `ui.appearance` decodes, defaults, rejects unknown values with the field path, and appears in the schema, template and generated bindings; `bun run check:generated` passes.
- [x] UI-02-04: No code path forces dark; the mapping from `appearance` to the native theme is unit tested, and a reload re-applies it to live windows (fake-proven).
- [x] UI-02-05: An `m.*` component animates inside `MotionProvider`; a full `motion.*` component inside it throws; with reduced motion requested, transform animations are skipped.
- [x] UI-02-06: `Icon` renders an SVG at the default size in `currentColor`, hidden from assistive technology without `label` and named with it. The production bundle contains only the icons that are imported.
- [x] UI-02-07: `docs/architecture/ui.md` and `docs/architecture/config.md` describe tokens, the styling rules, motion and `[ui] appearance`.
- [x] UI-02-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Token tests in WebKit with emulated color scheme for both modes, covering at least one token of every color role.
- Motion value agreement test; `MotionProvider` tests for strict mode and reduced motion.
- `Icon.test.tsx` for size, color inheritance, accessibility and the style prop.
- End-to-end `theme.spec.ts`: the placeholder window under emulated light and dark shows the Latte and Mocha surface and text colors, and switches when the emulation changes without a reload.
- Rust: config decode, default, rejection, template and catalog tests; appearance mapping; reload re-apply against the fake window backend.

## Notes

- Live verification (the webview inside the `NSPanel` following the panel's appearance, and a `global.toml` reload switching live windows) needs the user's go-ahead right before launch. Record it as unverified otherwise.
- Catppuccin values come from the published Catppuccin palette, not from the deleted token file.
- Outcome: Motion's `strict` check only runs where a `process` global exists, so the throw is proven in tests through a shim and does not fire in the webview; the lint restriction is the guard there. Native appearance (system follow, forced light/dark, live reload, the webview inside the panel) is fake-proven and not yet verified live.
