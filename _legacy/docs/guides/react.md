# React conventions (`app/ui`)

Read when you write or review UI code in `app/ui/`.

## Layout

The Vite root is `app/ui` and `@/` maps to it. `main.tsx` renders `Renderer`, which picks a
window root from `getCurrentWindow().label`: `bar` → `renderer/bar`, `widgets` →
`renderer/widgets`. Those labels come from `app/native/tauri.conf.json`.

Each window is a separate webview with its own React tree and its own `QueryClient`; they
share nothing in memory. Sharing happens through Zustand stores synced by
`@tauri-store/zustand`.

## Component folders

One folder per component, files named after it:

```text
Component/
  index.ts                 export { Component } from './Component';
  Component.tsx            component
  Component.styles.ts      Linaria css
  Component.types.ts       props and payload types (when needed)
  Component.state.ts       hook that produces the view model (when needed)
  Component.test.tsx       Vitest browser test
```

`renderer/bar/Status/Battery/` is the smallest complete example: `.state.ts` turns the store
data into a view model (icon, label, color, click handler), `.tsx` renders it, `.test.tsx`
mocks the store hook.

## Styling

- Linaria `css` lives in `*.styles.ts`; the Vite `wyw` plugin only transforms files matching
  that glob, and its `importOverrides` merely allowlists imports from the design system and
  a few utility modules. A `css` block in another file is not transformed.
- Import tokens from `@/design-system`: `colors` (Catppuccin Mocha — `base`, `mantle`,
  `crust`, `surface0..2`, `text`, `subtext0/1`, `overlay0..2`, the accents, plus
  `crustFaded` and `crustTransparent`) and `motion` (`easing`, `duration` 150 ms,
  `durationSlow` 400 ms) or `motionRaw` (numeric seconds for `motion` timing objects; its `durationSlow`
  is 0.275 s, not 0.4 s, and it adds `durationSlower` 0.5 s).
- Compose classes with `cx(styles.a, cond && styles.b)` from `@linaria/core`.
- Do not hand-write hex colors or durations that the design system already defines.

## Hooks (`app/ui/hooks`, exported from `index.ts`)

| Hook                   | Purpose                                                                                                                                       |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `useTauri`             | React Query query from `command`/`commandArgs` (or `queryFn`), optional `eventName`/`eventTransform`, `syncAcrossWindows` defaulting to true. |
| `useTauriSuspense`     | Same options on top of `useSuspenseQuery`. Stores use this variant.                                                                           |
| `useTauriEvent`        | Subscribes to one event, optional `targetWindowLabel`, unsubscribes on unmount.                                                               |
| `useCrossWindowSync`   | Backs `syncAcrossWindows`: mirrors query data into a Zustand store keyed by the hashed query key.                                             |
| `useWidgetToggle`      | Ref plus click handler that emits `stache://widgets/toggle` with the trigger's rect to the `widgets` window.                                  |
| `useMediaQuery`        | `matchMedia` subscription; shared queries such as `LAPTOP_MEDIA_QUERY` live in `utils/media-query.ts`.                                        |
| `useDisableRightClick` | Suppresses the context menu (except in dev) on the bar and widget windows.                                                                    |

## Stores

`app/ui/stores/<Name>Store/` groups a data hook, its types and icon helpers; only
`BatteryStore` and `WeatherStore` exist. A store is not global state — it is a thin wrapper
over `useTauri`/`useTauriSuspense` with a stable `queryKey` and, where the source is a
system API, a `refetchInterval` (battery polls 30 s while charging, 2 min otherwise).

Rules:

- `queryKey` must be a stable array and identical in every window; it is hashed into the
  cross-window store id.
- Cross-window consistency comes from `useCrossWindowSync` plus `utils/createStore.ts`
  (`createStore`, `getStore`, `destroyStore`, `getStoreIds`).
- Weather provider selection and geolocation live under `WeatherStore/providers/` and
  `WeatherStore/location/`.

## Events and commands

- Event constants come from `@/types` (`app/ui/types/tauri-events.ts`), which mirrors
  `app/native/src/events.rs`. Never inline a `stache://…` string.
- Commands are named on the hook (`command: 'get_battery_info'`) and must be registered in
  `app/native/src/lib.rs`.
- `emitTauriEvent` sends to another window; `invoke` and `listen` come from
  `@tauri-apps/api`.

## Shared components

`components/` holds the primitives: `Stack`, `Surface` (motion variants in
`Surface.motion.ts`), `Button`, `Icon`, `ScrollingLabel`, `QueryErrorFallback`.

Each window root wraps its tree in `QueryClientProvider` → `ErrorBoundary` → `Suspense`
and owns its `QueryClient` (`Bar.tsx` and `Widgets.tsx` create one from
`utils/queryClientDefaults`). Do not hoist it to `main.tsx`, where it would be shared by
both windows.
