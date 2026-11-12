# UI

Read when changing the React UI in `src/ui/`, its toolchain or test harnesses, the
typed command/event surface, the generated TypeScript bindings, or the windows the
host opens for the UI.

The UI was rebuilt from scratch on the [UI foundation](../specs/2026-11-11-ui-rebuild/00-overview.md)
(toolchain, tokens, data layer, Bar layout engine) and the
[migration](../specs/2026-11-11-ui-migration/00-overview.md) (the Bar shell, its eight
widgets, the Accessibility guide and the test window). `bar` renders the Bar window,
`access` the Accessibility guide, `test` the test window; `shell` keeps a placeholder
as the development fallback.

## Stack

React 19 with the React Compiler, TypeScript, Vite 8 and StyleX 0.19
(`@stylexjs/unplugin`), TanStack Query, zustand,
Motion for React (`motion` 14), Hugeicons free (`@hugeicons/react`,
`@hugeicons/core-free-icons`), `@icons-pack/react-simple-icons` (13.15.1, exact:
brand icons Hugeicons free lacks) and `react-error-boundary` (used by the app shell).

## Layout

```text
src/ui/
  index.html              Vite entry; loads /src/main.tsx
  e2e/                    Playwright Test: fake host, fixtures, scenarios/*.spec.ts
  tests/                  Vitest setup and helpers: setup.ts, render.tsx, ipc.ts, fixtures.ts, barSnapshot.tsx
  src/
    main.tsx              bootstrap only: imports main.css, StrictMode root, <App />
    main.css              CSS entry: layer order, reset, transparent html/body/#root
    router.ts             ?surface= -> lazily loaded window (no JSX)
    app/                  App, AppShell (error boundary + Suspense)
      providers/          Providers (composition), MotionProvider/, QueryProvider/ (with the query client)
      features/           lazily loaded feature bundles (motion.ts: domMax)
    ipc/                  the only code that may import @tauri-apps/* or the bindings
      bindings.gen.ts     generated, never edited
      window.ts           closeCurrentWindow(), dragRegion (window-level Tauri APIs)
      index.ts            re-exports commands, events, DTO types, subscribe and window.ts
    queries/              feed.ts, host.ts, bar.ts, barActions.ts and access.ts (command mutations)
    hooks/                useMediaQuery (compact mode)
    stores/               createStoreContext (scoped zustand stores), <store>/ (ticker/)
    utils/                errorMessage (text of a thrown value)
    theme/                palette.stylex.ts, tokens.stylex.ts, motion.ts
    components/<Name>/    shared components (Icon, ScrollingLabel)
    windows/<Name>/       one directory per window (Bar, AccessibilityGuide, TestWindow,
                          Placeholder, UnknownSurface)
      Bar/                Bar.tsx, Bar.style.ts, Bar.css, Bar.test.tsx, index.ts
        components/       Bar-only components (BarItem/)
        widgets/          registry.ts (registry + its context), types.ts, resolve.ts,
                          index.ts, WidgetSlot/, one folder per widget
        providers/        WidgetRegistryProvider/
        styles/           widgetIcon.style.ts (shared widget icon styles)
        utils/            nodes.ts (persistent DOM node per item id) + test
      AccessibilityGuide/ AccessibilityGuide.tsx, .style.ts, .test.tsx, .state.ts (+ test), index.ts
        components/       HeroBadge/, GuideSteps/, IdentityCard/, Troubleshooting/, ActionFooter/
      TestWindow/         TestWindow.tsx, .style.ts, .test.tsx, .labels.ts (+ test), index.ts
        components/       StateBadge/, ModuleList/, ConfigList/
        styles/           card.style.ts (section, list, card and callout styles)
  e2e/
    fakeHost.ts, fixtures.ts, harness.html, harness/ (main.tsx, BarItemGallery.tsx)
    scenarios/            Playwright scenarios
    screenshots/          Playwright screenshot specs, font.ts, __screenshots__/ baselines
```

Dependencies point one way: `windows/Bar` → `windows/Bar/providers` →
`windows/Bar/widgets` → `windows/Bar/components`, `windows/Bar/styles` → `components` → `theme`; widgets also read `queries`, `stores` and `hooks`;
`queries` → `ipc`; `stores` never import `ipc`, `queries` or widgets. The
Accessibility guide and the test window read `queries`, `ipc` (window wrappers) and `utils`.

## Conventions

- **Directories.** Each component or window directory holds `<Name>.tsx`,
  `<Name>.style.ts` (exports `stylex.create` styles), `<Name>.test.tsx` and `index.ts`.
  A component that renders no element of its own (`Providers`,
  `WidgetRegistryProvider`, `WidgetSlot`) has no style file. No component lives
  as a flat file. Directories exist only when they have content.
- **Window-local folders.** A window may have its own `components/`, `widgets/`,
  `providers/`, `styles/` and `utils/` folders for code only it uses (the Bar
  does); code shared by several windows lives under `src/`.
- **Screenshot tests** live in `src/ui/e2e/screenshots/` (Playwright), never next
  to source.
- **Alias.** `@/` → `src/ui/src/`, defined once in `tsconfig.app.json` (`paths`). Vite
  reads it with `resolve.tsconfigPaths`; `vite.config.ts` passes the same table, as
  absolute paths, to StyleX `aliases` (with
  `unstable_moduleResolution: { type: 'commonJS', rootDir }`), so `defineVars` imports
  resolve the same in dev, build and tests.
- **Plugins.** `vite.config.ts` runs `stylex.vite(...)` first, then
  `@vitejs/plugin-react`, then the React Compiler Babel preset. Hand-written
  `useMemo`/`useCallback`/`memo` are not used.
- **Cascade layers.** StyleX runs with
  `useCSSLayers: { before: ['reset'], prefix: 'stylex' }`; `main.css` declares
  `@layer reset, stylex;`. The reset sits below every `stylex.priority*` layer. The
  build appends StyleX's CSS to the single CSS asset emitted for `main.css`.
- **Dev stylesheet.** With `devMode: 'full'` the StyleX plugin injects
  `/virtual:stylex.css` and the `virtual:stylex:runtime` script into `index.html`
  (`transformIndexHtml`), so dev and the end-to-end harness load StyleX output without
  hand-written tags.
- **IPC boundary.** oxlint `no-restricted-imports` rejects `@tauri-apps/*` and
  `**/bindings.gen` imports anywhere in `src/ui/` except `src/ui/src/ipc/`,
  `src/ui/tests/` and `src/ui/e2e/`; those paths keep the full-`motion`,
  `framer-motion` and raw-palette restrictions. `scripts/ui-lint-boundaries.test.ts` proves these rules.
- **Style linting.** Stylelint cannot read StyleX objects, so its rule sets are
  split by what they lint. StyleX style objects go through oxlint `jsPlugins`:
  `@stylexjs/eslint-plugin` (`valid-styles`, `valid-shorthands`,
  `no-nonstandard-styles`, `no-conflicting-props`, `no-unused`,
  `enforce-extension`, and `sort-keys` with `order: 'clean'`, the
  `stylelint-config-clean-order` property order, fixable with `oxlint --fix`)
  and the local plugin `scripts/lint/stylex-animation.js`, whose
  `stache-stylex/no-low-performance-animation-properties` carries over
  `stylelint-high-performance-animation` with paint properties allowed: no
  layout-triggering property (or `all`) in `transitionProperty`, `transition` or
  `stylex.keyframes`. Real CSS files (`main.css`) are linted by stylelint with
  `stylelint.config.mjs` (recommended, clean order with blank lines between
  groups, the animation plugin). The same tests prove the order and animation rules.
- Build target stays `safari26`.

## Tokens and themes

Token files are `src/ui/src/theme/*.stylex.ts` and export only `defineVars` /
`defineConsts` results. Two tiers:

- **Palette** (`palette.stylex.ts`): the published Catppuccin Latte and Mocha
  colors as `defineConsts` (`latte`, `mocha`). Only files under
  `src/ui/src/theme/` may import it: oxlint `no-restricted-imports` rejects it
  everywhere else in `src/ui/` (proven by `scripts/ui-lint-boundaries.test.ts`).
- **Semantic** (`tokens.stylex.ts`): `defineVars` groups components use —
  `color` (`surface`, `surfaceRaised`, `surfaceSunken`, `surfaceHover`,
  `surfaceActive`, `text`, `textSecondary` (subtext1), `textMuted`, `textSubtle`,
  `textFaint` (overlay2), `border`, `divider` (surface0), `focus` (lavender),
  `accent`, `onAccent`, `success`, `warning`, `danger`, `attention` (peach),
  `info`, `highlight`), `space` (`none` to `xxxl` 32px), `radius`, `font` (family
  `'Maple Mono NF', 'SF Mono', monospace`, `familySystem` and `familySystemMono`
  (the system stacks of the test window), sizes, weights, line heights; fonts are
  system installed, never bundled), `shadow` and `size` (control and icon
  sizes), `guide` (the Accessibility guide's own text style in the Bar typeface:
  13px, weight 500, line height 1.6em, 17px titles, 12.5px steps; 16px panel and
  12px control radii, 48px header, 36px buttons, 44/64px hero badges),
  `statusPage` (the test window: 13px/1.5 system text, 20px title, a 54rem column,
  8px cards) and `bar` (the Bar text style and item shape: `fontSize` 13px,
  `fontWeight` 500, `lineHeight` 1.6, `itemRadius` 12px, `itemPaddingInline`
  10px, `itemBorderWidth` 1px, `labelFadeWidth` 10px, `iconSize` 18px (widget
  icons), `labelMaxWidth` 256px and `labelMaxWidthCompact` 160px (the widest
  a widget's text label grows before it scrolls), and Media's `mediaIconSize`
  22px, `mediaArtworkSize` 24px, `mediaLabelMaxWidth` 560px and
  `mediaLabelMaxWidthCompact` 400px). Every color is `light-dark(<latte>, <mocha>)`,
  composed at compile time from the palette consts.
- **Bar values.** Today's Bar (on `main`) maps onto the tokens as follows; dark
  is the Mocha value, light its Latte counterpart: item rest surface
  `color.surfaceSunken` (crust), item hover and active surface
  `color.surfaceActive` (surface1), item hover and active border `color.surface`
  (base), text `color.text`, status red/yellow/green `color.danger`/`warning`/
  `success`, sky `color.highlight`; the gap inside an item `space.sm` (6px) and
  between items in a zone `space.xs` (4px). Tabular numbers are
  `font-variant-numeric: tabular-nums` on the Bar window.
- **Static consts** (`tokens.stylex.ts`): `duration`, `easing`, `zIndex` and
  `media` (`reducedMotion`, `reducedTransparency`, `moreContrast`, `compact`)
  never theme. A `media` const works as a conditional style key
  (`{ default: x, [media.compact]: y }`); `theme/media.test.tsx` proves
  `media.compact` (viewport width) and `media.reducedMotion` apply. How it
  works, and the one setting it needs: the StyleX compiler emits a const as a
  placeholder (`var(--<hash>)`) in the consumer's rules and records the real
  value only in the defining file's rules; `processStylexRules` in
  `@stylexjs/babel-plugin` substitutes placeholders when it writes the CSS. In
  dev and Vitest, `@stylexjs/unplugin` serves `/virtual:stylex.css` from the
  modules transformed so far (`collectCss` in `lib/core.js`), so if a
  consumer is transformed before `tokens.stylex.ts` the at-rule stays
  `var(--…){…}` and lightningcss rejects the whole stylesheet ("Invalid empty
  selector") until the next update. `vite.config.ts` therefore warms up
  `src/theme/*.stylex.ts` (`server.warmup.clientFiles`), transforming them at
  server start. The build transforms every module first and was never
  affected. Script-side reads (`useMediaQuery`, Motion's `useReducedMotion`)
  remain for behavior that is not a style (Motion-driven animation).

Light/dark is CSS only. `main.css` sets `color-scheme: light dark` on `:root`; the
host sets the application-wide native appearance (`NSApp.appearance`) from
`[ui] appearance` in `global.toml` and every window and the Bar panel inherit it
(see [config](config.md)), so `prefers-color-scheme` in the webview is right at
first paint and switches live with the system. No JavaScript reads or sets the
theme. The StyleX plugin post-processes CSS with lightningcss; `vite.config.ts`
pins its targets to Safari 26 so `light-dark()` ships as is (the default
browserslist lowers it to a polyfill that breaks across stylesheets).

Each semantic group is the extension point for later custom themes:
`stylex.createTheme(color, {...})` (or a function style setting one var)
overrides it for a subtree without touching components. No production theme
switcher ships; `theme/tokens.test.tsx` proves a subtree override.

## Styling rules

- Components use semantic tokens only: no raw color or pixel literal where a
  token exists, and never the palette.
- A component's styles live in `<Name>.style.ts` (`stylex.create`).
- A component accepts `style?: StyleXStyles` (optionally restricted to a
  property set, as `Icon` does) and passes it **last** to `stylex.props`, so the
  caller's override wins.
- Conditional styles are `stylex.props` arguments (`active && styles.active`);
  runtime values use function styles (`sized: (px: number) => ({ width: px })`).

## Motion

`app/providers/MotionProvider/` is part of `Providers`: `LazyMotion` with `strict` and
the `domMax` features imported asynchronously from `app/features/motion.ts`,
wrapping `MotionConfig reducedMotion="user"` with the default transition.
Components use `import * as m from 'motion/react-m'`; oxlint rejects importing
`motion` from `motion/react` and any `framer-motion` import. `theme/motion.ts`
holds durations (seconds), cubic-Bézier easings and named transition presets;
the StyleX `duration`/`easing` consts hold the same values as CSS strings and
`theme/motion.test.ts` keeps them equal. Today's Bar motion: `easings.inOut`
(`cubic-bezier(0.42, 0, 0.58, 1)`), `durations.bar` (150 ms: slide-away and item
hover), `transitions.fadeIn` (275 ms opacity, item mount) and
`transitions.spring` (a spring with `bounce: 0` over 500 ms, for Spaces).
`scrolling` holds `ScrollingLabel`'s marquee values: speed (60 px/s) and the
gap between copies as a fraction of the box width (0.5). With reduced motion requested, Motion
skips transform animations.

Motion's strict check throws only where `process` exists (its development
guard), so in the webview it is silent; the lint rule is the real guard. Tests
turn it on with `tests/processShim.ts`.

## Icon

`components/Icon` draws an `IconSource`: a Hugeicons icon (through
`HugeiconsIcon` from `@hugeicons/react`) or a `@icons-pack/react-simple-icons`
component (an `Si*` brand, told apart because Hugeicons icons are element
arrays). Props: required `icon`, optional `size` (pixels; default the
`size.iconMd` token), `strokeWidth` 1.5 (Hugeicons only; simple-icons are
filled), `color` fixed to `currentColor` (stroke for Hugeicons, fill for
simple-icons), optional `label`. Without `label` the SVG is `aria-hidden` (a
simple-icon's own `<title>` is emptied); with it, it is `role="img"` named by
the label. Both kinds take the same class, so size and overrides behave the same. `style` accepts
color, `fill`, `width`/`height`, opacity, margin, `alignSelf` and paint
transition (`transitionProperty`/`Duration`/`TimingFunction`) overrides. Icons are imported by name at
each call site from `@hugeicons/core-free-icons` (or `@icons-pack/react-simple-icons`,
which is side-effect free); no file re-exports an icon set, so the bundle keeps only
imported icons (`scripts/ui-icon-treeshake.test.ts`).

## BarItem

`windows/Bar/components/BarItem` is the rounded item every Bar widget renders into. Props:
`onClick` (with it the item is a `button type="button"`, without it a `div`),
`active`, `label` (accessible name for icon-only items: `aria-label` on the
button, a named `group` on the div), `children` and `style` (restricted to
`color`, `columnGap`, `paddingInline`, `minWidth`, `maxWidth`, `width`, merged
last). The item is an inline grid (`columnGap` `space.sm`) of full Bar height
with `bar.itemPaddingInline`, a 1px border and `bar.itemRadius`. Rest:
`color.surfaceSunken`, transparent border. Hover (clickable items only) and
`active`: `color.surfaceActive` with a `color.surface` border; background and
border colors transition over `duration.bar` with `easing.inOut`. Keyboard
focus shows a 2px `color.accent` outline (`:focus-visible` only). It is an
`m.button`/`m.div` that fades in once on mount (opacity only,
`transitions.fadeIn`); a re-render never fades again. Reduced or disabled
motion comes from `MotionProvider`.

## ScrollingLabel

`components/ScrollingLabel` shows `children` (a string) on one line in a box whose width
the parent (or `style`: `width`, `minWidth`, `maxWidth`, `color`) limits. It
measures the text's width against the box's width (both fractional
`getBoundingClientRect` widths; a difference of 0.5px or less is rounding, not
overflow, so a box sized to its own text stays static; `ResizeObserver` on
both, and again whenever `children` change) and exposes the result as `data-state`:

- `static`: the text fits; no motion, no mask.
- `scrolling`: the text is wider than the box; it runs as a continuous
  marquee. With box width `b`, text width `t` and gap `g = scrolling.gap × b`
  (half the box width), one track (a Motion value `x` driven by the imperative
  `animate`, transform only) holds the text and, `g` after it, an
  `aria-hidden` copy whose text comes from `data-text` through CSS
  `content: attr(data-text)`, so assistive technology and `textContent` read
  the label once. The track enters once from `x = b` (just past the right edge)
  to `0` at `scrolling.speed`, then loops from `0` to `-(t + g)` in
  `(t + g) / speed` seconds forever; the wrap is seamless because the copy
  lands exactly where the text started. So the next copy starts entering when
  the leading copy's right end reaches the middle of the box, and the box is
  never empty after the intro. A change of text, box width or reduced motion
  stops the animation and restarts from the right edge; unmount stops it.
  While scrolling the box widens by `space.xs` on each side (negative inline
  margin, limited by `maxWidth: 100%`) and both edges fade over
  `bar.labelFadeWidth` (10px) through a `mask-image` gradient, so the fade
  matches any item background.
- `truncated`: it overflows under reduced motion (`useReducedMotion`); nothing
  moves and the text ends in an ellipsis.

## Compact mode

`hooks/useMediaQuery(query)` subscribes with `useSyncExternalStore`, one cached
`subscribe`/`getSnapshot` pair per query string, so it re-renders only when the
query flips. The compact (laptop) rules are `width <= 2036px`, defined once as
`media.compact` (`'@media (width <= 2036px)'`) in `tokens.stylex.ts`;
`COMPACT_QUERY` strips the `@media ` prefix from that const at runtime and
`useCompact()` reads it. Style files use `[media.compact]` as a conditional
key directly (see [Tokens and themes](#tokens-and-themes)); the hook is for
behavior.

## Router and app shell

`router.ts` exports the `surfaces` table, `DEFAULT_SURFACE` and `resolveRoute(search)`.
Every surface maps to a `React.lazy` window, so each window is its own chunk: `bar` maps
to `windows/Bar`, `access` to `windows/AccessibilityGuide`, `test` to `windows/TestWindow`
and `shell` to `windows/Placeholder` (the development fallback). Rules:

- `surface` absent → `shell`.
- `surface` empty or unknown → the local `UnknownSurface` fallback, which calls no native
  command. Matching is case-sensitive (`Bar` is unknown).
- A repeated parameter uses the first value.

`App` resolves `window.location.search` once and renders the window inside `AppShell`:
`Providers` (`QueryProvider` around `MotionProvider`), a top-level `ErrorBoundary`
whose fallback (`role="alert"`) shows the error message, and `Suspense`.

## IPC boundary

`src/ui/src/ipc/index.ts` re-exports the generated `commands`, `events` and every
DTO type unchanged, plus `subscribe(event, handler, { onReady?, onError? })`.
`subscribe` returns a synchronous dispose; when `listen` resolves after disposal
it unlistens at once and calls neither `onReady` nor the handler. Command
results that are typed outcome unions (`BarSnapshotOutcome`, ...) stay unions;
only a rejected `invoke` is an error. Everything else imports from `@/ipc`.
`ipc/window.ts` wraps the window-level Tauri APIs: `closeCurrentWindow()`
(`getCurrentWindow().close()`, never throws; `core:window:allow-close`) and
`dragRegion`, the `data-tauri-drag-region` attribute to spread on a drag region
(`core:window:allow-start-dragging`).

## Queries and feeds

- `app/providers/QueryProvider/client.ts` `createQueryClient()`: `networkMode: 'always'` (queries and
  mutations), `refetchOnWindowFocus: false`, `refetchOnReconnect: false`,
  `retry: false`, `staleTime: Infinity`. A query that reaches a remote service
  overrides these in its own options. `QueryProvider` (`app/providers/QueryProvider/`) creates one client per
  window and is part of `Providers`.
- Every query is a `queryOptions` factory, grouped per domain (`queries/host.ts`,
  `queries/bar.ts`). Components call `useQuery({ ...fooOptions(), select })`;
  no component writes a query key. Bar slices use the exported plain selectors
  (`selectBarView`, `selectBarKind`) so a component re-renders only for its slice.
- **Remote queries.** A query that reaches a remote service (Weather,
  `widgets/Weather/Weather.query.ts`) lives with its widget, keeps every secret
  (keys, coordinates) inside its `queryFn` closure and out of keys, cache
  entries, logs and error messages (thrown messages are fixed strings), passes
  the query `signal` to `fetch` and bounds each request with its own timeout.
  It sets its own refresh options (`refetchInterval`,
  `refetchIntervalInBackground`, `refetchOnReconnect: 'always'`). Tests stub
  `fetch` (Vitest) or route the provider hosts with `page.route` and abort any
  other remote request (Playwright); no test reaches the network.
- `appInfoOptions()` (`commands.appInfo()`) uses `staleTime: 'static'`: fetched
  once per client, never refetched while observed, even on invalidation.
- **Feeds** (`queries/feed.ts`). `createFeed({ options, snapshot, event, read,
  sequenceOf? })` keeps one query current; `useFeed(feed)` runs it for the
  component's lifetime (`feed.start(client)` returns the stop). Feed queries
  use `queryFn: skipToken`: only the feed writes them, so read them with
  `useQuery` (not `useSuspenseQuery`). Rules:
  - Subscribe first; request the snapshot only after `listen` resolved.
  - Sequences are decimal strings compared as `BigInt`, never `Number`. A value
    is applied only when its sequence is strictly greater than the held one; a
    malformed sequence is recorded as an error and ignored.
  - A value without a sequence (a feed without sequences, or Bar's
    `unauthorized`) is applied only while nothing sequenced is held and no event
    was applied since its snapshot was requested, so an event that arrives while
    the snapshot is in flight wins.
  - An event with `resnapshot` (and a newer sequence) requests a new snapshot.
  - `listen` and snapshot failures become the query's error state
    (`status: 'error'`, data kept); the next applied write clears it. A failed
    `listen` requests no snapshot; the next start subscribes again.
  - Starts are reference counted per `QueryClient` (a `WeakMap`): the first
    start subscribes, the last stop unlistens and drops in-flight snapshots.
    StrictMode mount, unmount, mount leaves exactly one listener.
- **Command mutations** (`queries/barActions.ts`). `barActionOptions()`,
  `barSessionActionOptions()` and `barTilingActionOptions()` are `mutationOptions`
  factories for `commands.barAction(action: BarAction)`,
  `commands.barSessionAction('keepAwake', action: SessionAction)` and
  `commands.barTilingAction(request: BarTilingRequest)` (the rendered
  `generation` and `sequence` plus a `focusWorkspace`/`focusWindow` target);
  each resolves with the typed outcome union. Components use one hook per
  command, which returns a runner: `useBarAction()(action)`,
  `useBarSessionAction()(action)`, `useBarTilingAction()(request)`. A call
  whose same action is still pending (checked synchronously in the mutation
  cache) is ignored, never queued: the same enum value for `bar_action`, any
  Keep Awake action for `bar_session_action`, the same target for
  `bar_tiling_action`. An outcome other than success (`completed`, `applied`,
  `applied`) and a rejected `invoke` are logged with `console.debug` only;
  nothing is drawn and no query is invalidated (the Bar changes only through
  the next `bar-changed`).
- **Accessibility mutations** (`queries/access.ts`). `requestAccessibilityOptions()`,
  `openAccessibilitySettingsOptions()` and `quitAppOptions()` are `mutationOptions`
  factories for `request_accessibility`, `open_accessibility_settings` and
  `quit_app`; `useRequestAccessibility()`, `useOpenAccessibilitySettings()` and
  `useQuitApp()` return the `useMutation` result. The guide disables a button
  while its mutation is pending and shows a rejected `invoke` in its alert line.
- `hostStatusFeed`/`hostStatusOptions`: `commands.hostStatus()` and
  `events.statusChanged` (no sequences). `barFeed`/`barSnapshotOptions`:
  `commands.barSnapshot()` and `events.barChanged`, ordered by `sequence`,
  honoring `resnapshot`; the cache holds the whole typed outcome.

## Bar layout and widgets

`bar.toml` `[layout]` (see [config](config.md) and
[bar.md](../features/bar.md#layout)) reaches the UI as `BarView.layout`:
`{ start, center, end }`, each an ordered list of `BarLayoutItem`
`{ id, widget, options }` with unique ids: a union tagged by `widget` whose
`options` are the resolved entry options of that widget (`{}` when it has none).

**Widget contract** (`src/windows/Bar/widgets/`, imported as `@/windows/Bar/widgets`
from outside the Bar window and as `./widgets` inside it; widgets belong to the
Bar window):

- `WidgetProps<K> = { id, zone, options }` (`zone` is
  `'start' | 'center' | 'end'`). `options` is `WidgetOptions<K>`, derived from
  the generated `BarLayoutItem` union (the `options` of the member whose
  `widget` is `K`), never redeclared: `WidgetProps<'clock'>['options']` is
  `ClockOptions`, a widget without options gets `NoOptions` (`{}`).
- A `WidgetDefinition<K>` is `{ load }`, a function returning a dynamic import
  of a `WidgetComponent<K>` (`() => import('./Clock')`), so each widget is its
  own chunk and a widget absent from the layout is never downloaded.
- `WidgetRegistry = { [K in BarWidgetKind]: WidgetDefinition<K> }` is
  exhaustive, so a definition only accepts a component for its own kind's
  options and a kind added in Rust fails type checking until it is registered
  (`widgets/registry.test.ts` proves it with `expectTypeOf` and a
  `@ts-expect-error` registry missing a kind). `productionRegistry` registers
  all eight kinds (`battery`, `clock`, `cpu`, `keep_awake`, `media`, `spaces`,
  `weather`, `wifi`), each its own chunk. `PartialWidgetRegistry` (=
  `Partial<WidgetRegistry>`) is what the context, `WidgetRegistryProvider` and
  `resolveWidget` accept, so tests and the harness may leave kinds out.
- `WidgetRegistryContext` (in `widgets/registry.ts`, the same module as the
  registry it carries) defaults to `productionRegistry`;
  `providers/WidgetRegistryProvider` supplies another one (tests, the end-to-end harness).
- `resolveWidget(registry, kind)` returns the definition, widened to accept any
  options (the caller passes it only items of `kind`), or `null` (in
  development an unregistered kind warns once per kind). `lazyWidget(definition)`
  creates the `React.lazy` component once per definition (a `WeakMap`); a
  rejected `load` evicts the entry, so a later mount (for example after a layout
  change) retries instead of leaving the kind blank until reload.
- `WidgetSlot({ id, widget, options, zone })` (a layout item plus its zone)
  renders the lazy widget with `{ id, zone, options }` inside its own
  error boundary and `Suspense` (both with a `null` fallback). A widget that
  throws or whose `load` rejects renders nothing, is logged once with
  `console.error` and leaves the other widgets working. The boundary is keyed by
  `widget`, so reconfiguring an `id` to another kind recovers a failed slot and
  starts the new kind with fresh state. An options-only change re-renders the
  same instance with the new options and keeps its state. Each mount keeps one
  lazy component, so a failed load is retried only by a later mount.

**Bar window** (`windows/Bar/`). The outer element (`data-bar-window`) carries
the Bar window's root rules, so they apply to this window only and `main.css`
stays generic: fixed to the window, full height, `overflow: hidden`, the font
stack with the `bar` text style and tabular numbers, `color.text`,
`user-select: none`, and `onContextMenu` prevented (no context menu, in
development too). `Bar.css` (in the `reset` layer, scoped to
`[data-surface='bar']`) makes images and SVGs blocks with
`-webkit-user-drag: none`. Inside it, the Bar grid (`data-surface="bar"`) is an
`m.div` that **slides away** while `selectBarAway` is true, which is only for an
`available` snapshot whose `menu.state` is `visible`: it translates down by its
own height (`y: '100%'`) and fades to opacity 0 over `durations.bar` with
`easings.inOut`, and returns for every other outcome or menu state (also before
the first snapshot). Under reduced motion both moves are instant. While away it
is `aria-hidden` and marked `data-away`.

**Layout engine** (`windows/Bar/`): `Bar` starts `barFeed` with `useFeed` and
reads `layout` and `height` with `useQuery` + `selectBarLayout`. The
root is a three-column grid (`minmax(0, 1fr) auto minmax(0, 1fr)`), so the
center zone is centered on the Bar whatever the widths of the other two; start
items hug the leading edge and end items the trailing edge. Items in a zone
are separated by the item gap (`space.xs`, 4px). `height` sets the row height (StyleX function style); `padding` is not used by the UI because the host already insets the panel frame by it. Before
the first snapshot and while `unavailable` or `unauthorized`, the three zone
containers render with no items and no error UI. Items whose kind has no
definition are filtered out, so they leave no element.

Instances survive layout changes, including a move to another zone. Each item's
widget is rendered from **one keyed list** (`WidgetHost`, keyed by `id`) as a
portal into a persistent DOM node per id (`utils/nodes.ts`). Each zone renders an
animated box per item (`ZoneSlot`, an `m.div` inside `AnimatePresence` with
`mode="popLayout"`, `layout="position"` and opacity presence using
`transitions.standard`; `{ duration: 0 }` under reduced motion) that appends
the item's node while it is present. Moving an item to another zone mounts a
new box and moves the node into it, shown at full opacity (no fade-in for an
id that was already on the Bar); the React widget instance never changes
parent, so its state is kept. An exiting box never claims the node back. A
removed item's widget stays mounted in its node, inside the exiting box, until
that box's exit animation completes (at once under reduced motion); then it
unmounts and the node is released. Only the exit of the box in the zone the
item was removed from ends it: an item that moved zones and was then removed
still has an older box fading in its first zone, and that box's exit does not
unmount the widget. An id re-added before its exit completes
keeps its single instance and moves to its new box.

**How to add a widget** (migration spec):

1. Create `src/windows/Bar/widgets/<Name>/` with `<Name>.tsx` (default and
   named export of a component taking `WidgetProps<'<kind>'>`), `<Name>.style.ts`
   (only if it has styles of its own), `<Name>.test.tsx` and `index.ts`; pure
   logic (selection, formatting, icon and tone choice) goes in a plain
   `<Name>.state.ts` (or `<Name>.format.ts`) next to it, tested directly.
   Render into `BarItem` (`windows/Bar/components/BarItem`); read the snapshot only through
   `useQuery({ ...barSnapshotOptions(), select })` with a module-level selector
   returning just this widget's reading, so another reading's change does not
   re-render it (a widget that renders a list, like Spaces, selects only the
   item identities and lets each item select its own data, so a change re-renders
   only the items it touches); icons use `styles/widgetIcon.style.ts` (`bar.iconSize`, stroke 1.8,
   tone styles); clicks use the hooks in `queries/barActions.ts`.
2. Register it in `src/windows/Bar/widgets/registry.ts`:
   `clock: { load: () => import('./Clock') }`.
   Never import the widget statically anywhere else, or it leaves its chunk.
3. If it needs options, declare them in Rust (`layout.rs`): a
   `<Kind>Options` struct (snake_case fields, `#[schemars(default)]`, a
   `Default` with the documented defaults, `range`/enum types for bounds), used
   by its `BarLayoutEntryTable` variant (`BarEntryFields<<Kind>Options>`) and
   its `BarLayoutItem` variant; add each key to `option_owner` and decode it in
   place in `EntryVisitor::option` (a checking newtype for a bound, so errors
   keep the key's path), and range-check it in `validate`. Option names must be
   unique across widgets. Regenerate schema and bindings; `WidgetProps<K>`
   picks the options up from the generated union with no UI type change.
4. Cover it with a component test and, where it has host data, an end-to-end
   scenario through the fake host.

## Client state (zustand)

`stores/createStoreContext(createState, name?)` returns `Provider` (one vanilla
store per mounted provider), `useStore(selector)` (throws a clear error outside
the provider) and `useStoreApi()` for imperative access. Conventions:

- zustand holds client-only UI state, never host or remote data (that is
  TanStack Query's).
- State and actions are separate keys: actions live in one `actions` object, so
  selecting them is stable.
- Select one value per hook, or use `useShallow` for an object or array.
- Stores are scoped to a window through the provider unless they are truly
  app-wide.

**Layout.** `stores/createStoreContext.tsx` (and its test) stay at the root
of `stores/`. Each store gets its own folder named after it, holding the store
file and its test and no index file (`stores/ticker/ticker.ts`,
`stores/ticker/ticker.test.ts`; `.tsx` only for a file with JSX).
`stores/index.ts` re-exports every store and `createStoreContext`; consumers
import from `@/stores`.

**Ticker** (`stores/ticker/`): the one app-wide store, a vanilla zustand
store `{ now }` (epoch milliseconds) that every Clock shares. Its `subscribe`
counts subscribers: the first one refreshes `now` and schedules a `setTimeout`
to the next whole second; each tick reads `Date.now()` and re-aligns to the
next whole second (no drift); the last unsubscribe clears the timer, so no
timer runs while no Clock is mounted. `useTicker(selector)` reads it; a Clock
selects the start of the current second, or of the current minute with
`show_seconds = false` (computed from a `Date`), so it re-renders once a
second or once a minute. `now` is a moment; the Clock formats it with
`Intl.DateTimeFormat`, never by arithmetic. `createTicker()` makes an
independent ticker (tests). The sample scoped store lives in
`stores/createStoreContext.test.tsx`.

## Test harnesses

- **Vitest** (`bun run test:ui`, `bun run test:ui:coverage`): browser mode in Playwright
  WebKit, project `ui`, colocated `*.test.ts(x)` under `src/ui/src/` and `src/ui/tests/`.
  `tests/processShim.ts` runs first (enables Motion's development checks);
  `tests/setup.ts` imports `main.css`, sets `MotionGlobalConfig.skipAnimations`
  before each test (a test that needs real animation turns it off), cleans up,
  clears Tauri mocks and resets emulated media after each test. The browser
  command `commands.emulateMedia({ colorScheme, reducedMotion })` (from
  `vitest/browser`, defined in `vite.config.ts`) is Playwright
  `page.emulateMedia`. Motion reads `prefers-reduced-motion` once per page and
  WebKit does not report emulated changes to it, so a test file that needs it
  calls `installReducedMotionQuery()` from `tests/reducedMotion.ts` at top level
  and drives the value with `setReducedMotion(reduce)` (it fires `change`, as
  the system setting does); `tests/render.tsx`
  renders inside `Providers`; `tests/ipc.ts` `recordIpc()` replaces the IPC with
  `mockIPC` and records every call, and `mockHost()` is a scripted host over
  `mockIPC` with a live event registry (`answer`, `callsOf`, `emit`,
  `listenerCount`, `holdListen`/`releaseListen`, `failListen`);
  `tests/fixtures.ts` has typed `HostStatus`, `BarSnapshotOutcome` and
  `BarChanged` fixtures; `tests/barSnapshot.tsx` `renderWithSnapshot(ui,
  outcome)` renders a widget with the Bar snapshot query seeded as `barFeed`
  writes it and returns `setSnapshot`/`setView` (a newer reading) and
  `rerender` (same client and instance). Coverage thresholds: 80 lines / 80 functions /
  65 branches / 80 statements.
- **Playwright Test** (`bun run test:e2e`, `playwright.config.ts` at the root): WebKit
  only, scenarios in `src/ui/e2e/scenarios/*.spec.ts`, output in `test-results/e2e/`
  (ignored). `webServer` starts the Vite dev server on port **1430**, never 1420, so it
  cannot collide with `tauri:dev`. `e2e/fixtures.ts` provides `host` and
  `consoleErrors`. `e2e/fakeHost.ts` installs `window.__TAURI_INTERNALS__` and
  `__TAURI_EVENT_PLUGIN_INTERNALS__` with an init script before any page script (the
  window label follows `?surface=`). Every `invoke` goes through a Playwright binding to
  the Node-side `FakeHost`, which records it (`host.calls`, `host.commands()`), answers
  from the per-test script (`host.answer(command, valueOrFn)`, `host.fail(command,
  message)`) and rejects unscripted commands (listed in `host.unanswered`). Event
  listeners live in the page; `host.emit(event, payload)` delivers to them. Typed
  helpers: `answerHostStatus`, `answerBarSnapshot`, `emitStatusChanged`,
  `emitBarChanged`.
- **Harness page** (`src/ui/e2e/harness.html`, entry `e2e/harness/main.tsx`):
  boots the same `App` inside `WidgetRegistryProvider` with the fixture registry
  (`tests/fixtureRegistry.ts`: counter (it shows its options in
  `data-options`), label, a `BarItem` action widget for `media` (its click
  sends `bar_action("media")`), throwing and failing-load widgets;
  `keep_awake` unregistered). Open it as `/e2e/harness.html?surface=bar`. Only the
  dev server serves it; the build's single input is `index.html`, so `dist/` never
  contains it. `tsconfig.app.json` type-checks `e2e/harness/` (it is browser
  code), `tsconfig.e2e.json` excludes it. `tests/fixtureRegistry.ts` and
  `tests/widgets.tsx` are shared with the component tests.
  `/e2e/harness.html?gallery=bar-item` instead renders `BarItemGallery`
  (`e2e/harness/BarItemGallery.tsx`): a rest and an active `BarItem`, each in a
  framed `shot-rest`/`shot-active` element, for the screenshot specs.
- **Window scenarios**: `e2e/scenarios/access.spec.ts` (blocked, request,
  error, revoked, open and self-close; unbundled, Escape, Close, Quit),
  `test-window.spec.ts` and `bar-session.spec.ts` (a whole Bar session with the
  production registry and the default layout). `tests/ipc.ts`
  `mockCurrentWindow(label)` gives Vitest a current window for `closeCurrentWindow`.
- **Screenshot specs** (`src/ui/e2e/screenshots/`): see
  [Testing](../guides/testing.md) (screenshot baselines).

## Versions

| Dependency          | Pin            | Why                                                                |
| ------------------- | -------------- | ------------------------------------------------------------------ |
| `tauri`             | `=2.12.5`      | Host pin; `tauri-specta` enables its `specta` feature.             |
| `tauri-build`       | `=2.7.4`       | Build crate; `tauri 2.12.5` requires `tauri-build ^2.7.4`.         |
| `tauri-specta`      | `=2.0.0-rc.26` | Documented Tauri 2 binding tool.                                   |
| `specta`            | `=2.0.0-rc.26` | Exact version required by `tauri-specta` and the exporter.         |
| `specta-typescript` | `=0.0.13`      | TypeScript exporter used by the generator.                         |
| `@tauri-apps/api`   | `2.12.4`       | Runtime imported by generated bindings; must match the core minor. |
| `@tauri-apps/cli`   | `2.12.4`       | Build/dev CLI; it drives the same 2.12 core.                       |

`tauri-specta =2.0.0-rc.26` declares `specta =2.0.0-rc.26` and
`specta-typescript ^0.0.13`, so the resolved versions are
`specta 2.0.0-rc.26` and `specta-typescript 0.0.13`. Do not upgrade one of them
independently: newer `specta` and `specta-typescript` releases do not resolve until
`tauri-specta` moves to them. `tauri` also enables its `specta` feature so `tauri::State`,
`AppHandle` and window arguments are omitted from generated command parameters.

## Shared collection

`stache_app::bindings::builder()` owns the command and event collection. The
desktop host and the generator both use it, so there is no second list to keep in
sync:

```rust
let bindings = stache_app::bindings::builder();
tauri::Builder::default()
    .invoke_handler(bindings.invoke_handler())
    .setup(move |app| {
        bindings.mount_events(app);
        Ok(())
    })
```

Commands are concrete Rust functions (`collect_commands!`); events have concrete
payloads (`collect_events!`). Nothing goes through a generic JSON `Value` facade,
and there is no hand-written event-name file: generated `events.statusChanged` is
the only client-side name source.

The shipped surface is:

```text
commands.appInfo()      -> AppInfo       static product name, version, identifier
commands.hostStatus()   -> HostStatus    last published snapshot, no module locks
events.statusChanged    -> StatusChanged { status }
```

`host_status` reads the host's published snapshot cell, so it never waits for a
module, a worker join or a socket operation.

## Host status DTOs

- `HostStatus { ready, error, access, modules, config }`.
- `AccessView { state, revoked, appPath }` is the Accessibility launch gate: `state` is
  `checking`, `blocked` or `open`, `revoked` is true when trust was lost after being open in this
  launch, and `appPath` is the enclosing `.app` path of the running executable (`null` for an
  unbundled binary). A gated host is `ready: true` with `access.state = "blocked"`.
- `ModuleStatusView { id, state, enabled, paused, revision, detail, diagnostic }`.
  `state`/`detail` are runtime facts; `diagnostic` is a rejected configuration.
  `revision` is an exact decimal **string**: JavaScript numbers cannot hold every
  SDK `u64`. Treat it as opaque text and never call `Number()` on it.
- `ConfigFileView { file, path, state, active, diagnostic, warnings }` separates
  file validity (`missing`, `valid`, `invalid`) from active runtime bytes.
- `ModuleState` is `stopped`, `disabled`, `running`, `paused`,
  `waitingForPermission`, `unavailable`, `failed`.

A rejected reload keeps the running module's `state` and records `diagnostic`; it
does not become `failed`.

## Config types

`GlobalConfig` and `ProxyAudioConfig` are exported with explicit `.typ::<T>()`
calls from their authoritative crates. Specta derives live on those types only;
the generated file is never hand-mirrored.

The pilot's read-only listing DTOs — `AudioDeviceInfo`, `AudioListKind` and
`AudioListParams` — are exported from the same builder. They describe the
`proxyAudio.list` socket request and response, but the pilot has no Tauri
command and no UI module: launch still creates no window and no webview.

`WallpapersSetParams`, `WallpaperSelection`, `WallpaperScreen`,
`WallpapersSetOutcome`, `WallpapersOutcomeStatus`, `WallpapersOutcomeCause`,
`WallpapersTargetOutcome`, `WallpapersTargetResult` and
`WallpapersTargetFailure` are exported the same way. They describe the
`wallpapers.set` socket request and its typed domain outcome; no Tauri command
or UI surface consumes them yet.

`TilingConfig` and its nested wire types (`LayoutType`, `EasingType`,
`AnimationConfig`, `MasterConfig`, `MasterPosition`, `WorkspaceConfig`,
`WindowRule`, `GapsConfigValue`, `GapsConfig`, `ScreenGapsConfig`, `GapValue`,
`FloatingConfig`, `FloatingPosition`, `FloatingPreset`, `DimensionValue`,
`BordersConfig`, `BorderStateConfig`, `GradientConfig` and
`BorderAnimationConfig`) are exported the same way. They are catalog types for
the Tiling configuration; the shipped Tiling runtime consumes them when `enabled = true`.

Serde phases make `ProxyAudioConfig` and `AudioDevicePriority` unions of
`_Serialize` and `_Deserialize` shapes. Unified serde mode is rejected because
`skip_serializing_if` cannot be represented there. The JSON Schemas remain the
validation authority; the TypeScript types describe the wire shape.

## Generate and check

```sh
bun run generate-bindings
bun run generate-bindings -- --check
```

The generator is the Rust binary
`cargo run -q -p stache-app --features dev-tools --bin generate-bindings`. It writes
`src/ui/src/ipc/bindings.gen.ts`. `--check` exports to a secure
`tempfile::NamedTempFile`, compares bytes, prints the stale path and exits
nonzero without modifying the committed file or source. `bun run check:generated`
runs both the schema and binding checks; `bun run lint` and `bun run build` call
it, so CI fails on drift.

The generated file is excluded from oxfmt, oxlint and lint-staged: specta emits
tabs and its own layout. The generator only trims trailing line whitespace before
writing or comparing. Never hand-format or edit it; regenerate it instead.

## Test window

Launch creates no windows and no Dock icon (`ActivationPolicy::Accessory`). The
tray's **Open Test Window** action is the only launch path that creates a webview:

- Rust opens one `Standard` window through `NativeWindows`, labelled
  `stache-test-<n>` where `<n>` is a monotonic host id.
- The URL is the app URL plus `?surface=test`; `src/ui/index.html` remains the
  Vite entry, so the router (`src/ui/src/router.ts`) reads `surface` from the query string. There is no
  arbitrary URL, JavaScript URL or native flag input.
- The `stache-app` crate defines `custom-protocol = ["tauri/custom-protocol"]`
  and `bun run tauri:build` passes it. Without that feature Tauri stays in dev
  mode, loads `devUrl`, and a packaged window has no document; a release build
  fails in `build.rs` instead of shipping that state. `smoke` implies it so the
  opt-in harness exercises the embedded bundle.
- Module and host requests are validated twice: the SDK capability checks the
  declared surface, kind and size, and `NativeWindows` repeats the check for
  host-owned requests. Only the shipped `test` and `access` surfaces are implemented; surface
  spellings must be ASCII letters, digits, `-` or `_`, so they cannot add a query
  parameter.
- The same owner/key request reuses and focuses the existing window; closing it
  removes the bookkeeping entry and allows a new window. Closing the last window
  only removes that state: Tauri's `ExitRequested` arrives with `code: None`
  there, so the handler prevents the exit without starting the host teardown.
  Module stop (including a failed start) releases every window owned by that
  module. Repeated requests share one lease; the last released handle closes the
  native window.
- `WindowKind::Launcher` returns `WindowError::UnsupportedKind` until a tool spec
  defines its native semantics. A module may only request a surface declared in
  its descriptor.
- `WindowKind::NonActivatingPanel` is landed for surface `bar` only; `bar` is
  admitted only as a panel, and `test`/`access` stay `Standard`-only. Bar's
  production factory is chained next to Tiling in `lib.rs` (default disabled,
  inert until `enabled = true`) and requests it; the `bar` route and the
  `bar-window` capability exist. A panel request must carry an explicit
  logical `origin` (a `Standard` request must not); size validation is the
  shared one. `stache-bar-<n>` is built undecorated, transparent, shadowless,
  not resizable, hidden at build, unfocused and unfocusable, accept-first-mouse,
  all Spaces, no theme of its own (it inherits the application appearance), not closable/minimizable/maximizable, drag-drop off,
  at the requested position and size. It **never** goes through Standard's
  `show()` + `set_focus()`. Tauri owns the window and the webview and keeps its
  window hidden forever; right after the build the `stache-macos` panel host
  (`panel.rs`, main thread only) creates a native `NSPanel` with
  `Borderless | NonactivatingPanel` **at init**, moves the webview into it, and
  configures `canJoinAllSpaces | stationary` (never
  `fullScreenAuxiliary`), the hover tracking area and `BelowNormalWindowLevel`
  (last), then orders it front without activating the app; the panel sets no
  appearance and inherits the application one (see
  [Tokens and themes](#tokens-and-themes)). No class of a Tauri
  or AppKit object is changed (a class swap broke KVO and crashed on close;
  see the live evidence in [testing](../guides/testing.md)). Hosted state lives
  in a main-thread registry keyed by label. `unhost` (webview back into Tauri's
  window, panel closed) runs before every Tauri close of the window; a
  `Destroyed` event for a still-hosted label only drops the state. Reposition
  queues the move of Tauri's hidden window (Tauri applies it asynchronously)
  and places the panel directly at the requested top-left frame, converted to
  AppKit coordinates with the height of `NSScreen.screens[0]` read at that
  moment; it never copies the hidden window's (still stale) frame.
  Focus on a panel is a no-op. The hosting was verified live on macOS 27.0
  (2026-11-09, committed host `377512b`: no activation on click, hover while
  another app is active, below normal windows, clean close; see the
  [part 03 evidence](../specs/2026-11-03-bar/03-panel-window.md)). No private SkyLight
  call is used.
- **Bar data bridge authorization (slice c).** `bar_snapshot` takes the actual
  calling `WebviewWindow` injected by Tauri; its label must be the live window held
  by owner `bar` (`stache-bar-<digits>`). The test, access, shell and any forged or
  stale Bar label get the typed `{"kind":"unauthorized"}`. A Bar-shaped label while
  Bar is not running gets `{"kind":"unavailable","reason":...}` (`disabled`,
  `paused`, `waitingForPermission`, `failed`, `starting`, `stopping`, `stopped`,
  `unavailable`) with `generation`/`sequence`, immediately and without stale data.
  The `bar-window`
  capability (`capabilities/bar.json`, label `stache-bar-*`) grants only
  `allow-bar-snapshot`, `allow-bar-action`, `allow-bar-weather`, `allow-bar-media-artwork` and `allow-bar-session-action` (all five commands
  are ACL-gated in the app manifest, and each also authorizes the calling label
  itself),
  `core:event:allow-listen` and `core:event:allow-unlisten`. `bar_action` takes a
  closed `BarAction` enum only (part 05 slice b); `bar_weather` returns the weather inputs (including the API key and
  coordinates) to this window only (part 06 slice B). `bar_session_action` pauses/resumes `keepAwake` only. `bar_media_artwork` returns the 128x128 artwork PNG (base64) for an `artworkId` of `BarView.media` (part 07 slice B). `queries/barActions.ts` wraps `bar_action`, `bar_session_action` and `bar_tiling_action`; the widgets call them on click. No close,
  drag, filesystem, shell, opener, HTTP or plugin permission is granted, and
  `app.security.capabilities` lists `bar-window` explicitly.
- `OwnedWindow::reposition(WindowFrame)` moves and resizes the exact live window
  of that owner and session on the main thread; a stale handle, another owner or
  a closed window is refused and a queued move is dropped if a close wins.
- Lifecycle phases are tracked in `stache_sdk::WindowPhases` and read through
  `OwnedWindow::phase()` / `wait_phase_change()`: `Requested` (queue accepted
  only), `Created` (native build returned), `Loaded` (page finished), `Failed`
  (create error, with reason; a page-load failure is only reported through the
  host `WindowEvents::page_failed` hook, because Tauri exposes no load-failure
  event), `Closing` and `Closed` (the `Destroyed` event). Phases never move
  backwards, so a late created/loaded callback after a close or failure is
  ignored.

**React window** (`windows/TestWindow/`). A read-only status surface: the
header (app name, "Host test surface", `identifier (vversion)`), "Application
identity unavailable: …" when `app_info` fails, and a Host section reading
"Reading host status…" until the feed writes, an alert "Host status
unavailable" when it fails, or "Host ready"/"Host starting" with a "Host error"
alert. Then **Modules** (or "No modules registered.") with one card per
module: id, state badge (`Stopped`, `Disabled`, `Running`, `Paused`, `Waiting for
permission`, `Unavailable`, `Failed`), Enabled, Paused and Revision rows, and
separately labelled **Runtime detail** (`detail`) and **Configuration
diagnostic** (`diagnostic`) callouts; and **Configuration** (or "No
configuration files.") with one card per file: name, `Missing`/`Valid`/`Invalid`,
path, whether it is active, **File diagnostic** and **Warnings**. `revision` is
rendered as the received text and never converted to a number. It calls only
`app_info`, `host_status` and the event listen. Its markup is what the opt-in
`stache-smoke` harness reads (see [testing](../guides/testing.md#native-host-smoke-opt-in)).

`src/crates/app/capabilities/test-window.json` grants the test labels only
`core:event:allow-listen`, `core:event:allow-unlisten` and
`core:window:allow-close`. `app.security.capabilities` lists it explicitly, so a
new capability file is not silently enabled. No filesystem, shell, dialog, HTTP,
media, weather, system or tray permission applies.

## Accessibility guidance window

While the Accessibility launch gate blocks, the host opens one more host-owned
`Standard` window, surface `access` (`?surface=access`), key `access`, 480×640
logical pixels (not resizable), labelled `stache-access-<n>`:

- Chrome comes from `chrome_for(surface)` in `windows.rs`, carried in
  `NativeWindowSpec`: `access` is frameless (`decorations(false)`), transparent,
  not resizable and keeps the native shadow; `test` keeps the decorated, opaque,
  resizable defaults. There is no title bar or traffic lights. Transparency needs
  Tauri's `macos-private-api` cargo feature and `app.macOSPrivateApi: true` in
  `tauri.conf.json` (app-wide; it authorizes no other private API). The page sets
  `html[data-surface='access']`, which makes `html`, `body` and `#root` transparent
  for this surface only.

- The runner opens it once per gated episode on each `Blocked` transition (first
  check or revocation) through `NativeWindows`; `window.set_focus` brings the
  Accessory app and the window forward, exactly like the test window. A user
  close does not reopen it in the same episode. The tray row `access-guide`
  (**Show Accessibility Help…**) opens or focuses it again; the next episode
  opens it automatically. On `Opened` the runner closes it through the release
  path, and the React surface also closes itself when `status-changed` reports
  `open`.
- A trusted launch and an ungated host (empty baseline) never create it, so the
  `webviews=0` startup record is unchanged on those paths. The startup record is
  written before the first permission check; a gated launch logs
  `access: guidance window requested` when the host opens the window.
- `NativeWindows` accepts exactly the `test` and `access` host requests; every
  other surface, kind or invalid size stays refused.
- Commands in `bindings::builder()`, all returning `()` immediately with results
  delivered through `status-changed`: `requestAccessibility()` (explicit request,
  downgraded to a check while open), `openAccessibilitySettings()` and
  `quitApp()` (`HostHandle::request_shutdown`).
- `src/crates/app/capabilities/access-window.json` grants `stache-access-*`
  labels only `core:event:allow-listen`, `core:event:allow-unlisten`,
  `core:window:allow-close`, `core:window:allow-start-dragging` (header drag) and
  `allow-request-accessibility`,
  `allow-open-accessibility-settings`, `allow-quit-app`. `build.rs` declares those
  three commands in the Tauri app manifest, so they are ACL-gated: the test
  window's capability does not include them and cannot call them. Their
  autogenerated permission files live in `src/crates/app/permissions/autogenerated/`.
  `app.security.capabilities` lists both capabilities explicitly.
- **React window** (`windows/AccessibilityGuide/`). It starts `hostStatusFeed` and
  reads `appInfoOptions()` and the host status (`select` derives the section, the
  app path and the error). Sections (`AccessibilityGuide.state.ts`): `checking`
  (no status yet, or `access.state = checking`: a large neutral badge that
  pulses with a spinning icon and "Checking Accessibility access…"), `blocked`
  (accent badge, "Stache Next needs Accessibility access" and its line, the
  three numbered steps, the identity card with name, bundle identifier (once
  `app_info` answers) and path ("not an app bundle" when `appPath` is `null`),
  and a collapsed **Troubleshooting** disclosure whose text names an older
  build's entry, or for an unbundled binary the launching app), `revoked`
  (attention badge, "Accessibility access was turned off", "Stache Next stopped
  all features until it is turned on again.") and `open` (success badge,
  "Access granted. Starting Stache Next…", no footer; the window then calls
  `closeCurrentWindow()`). `HostStatus.error` and any rejected command show in
  one alert line (`role="alert"`, joined with `; `). The footer holds **Request
  Access** (primary, full width), **Open System Settings** and **Quit Stache
  Next**, each disabled with a spinner while its mutation is pending. Chrome:
  one rounded panel (16px, `color.surfaceSunken`, an inset `color.divider`
  ring above the content) filling the transparent window (`html`, `body` and
  `#root` are transparent in `main.css`), a header whose left part is the drag
  region (app glyph and name, children `pointer-events: none`) with the Close
  button outside it, a scrolling body and the fixed footer. Escape (not a key
  auto-repeat) and Close call `closeCurrentWindow()`. The panel scales in and
  each section fades in (CSS keyframes, off under reduced motion). It calls
  only `app_info`, `host_status`, its three commands, the event listen and the
  window close.
