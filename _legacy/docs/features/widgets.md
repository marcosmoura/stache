# Widget overlays (widgets)

Read when changing the widget popover window, its anchoring/sizing, the close-on-click-outside tap, or when adding a widget to the overlay.

Rust paths are relative to `app/native/src/`; UI paths are relative to `app/ui/renderer/`.

## Purpose

The `widgets` window is a transient, non-activating overlay anchored under the bar that shows exactly one widget at a time: `calendar`, `battery`, or `weather`. It is the dropdown of the status bar, nothing more.

## Entry points

| Caller                                         | Entry                                                                                            |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| App startup, UI-critical phase (after the bar) | `lib.rs::load_base_modules` → `modules::widgets::init(app)`                                      |
| Any bar item                                   | `app/ui/hooks/useWidgetToggle.ts` → `emitTo("widgets", stache://widgets/toggle, { name, rect })` |
| Global mouse tap                               | `modules/widgets/window.rs::mouse_event_callback` → `stache://widgets/click-outside`             |

## Key types

- `Widgets.types.ts`: `WidgetNames = 'calendar' | 'battery' | 'weather'`, `WidgetConfig { name, rect }`, `Rect`, `WindowFrame`.
- `modules/widgets/window.rs`: `WindowFrame`, `ClickOutsideState`, `STATE: Mutex<ClickOutsideState>`.
- `Widgets.state.ts`: `useWidgets()` → `{ isOpen, isAnimatingIn, transition, contentRef, activeWidget }`.

## Rust responsibilities

`widgets::init` does three things and nothing else:

1. `set_window_sticky` — same private-`SLS`-space + non-activating subclass as the bar (`platform/window.rs`).
2. `set_window_always_on_top` — `CGWindowLevelForKey(20)` plus `always_on_top`, a much higher level than the bar's `CGWindowLevelForKey(8)`, so the panel sits above normal windows and above the bar.
3. `monitor_click_outside` — one process-global `CGEventTap` (listen-only) for left/right/other mouse-down, which emits `click-outside` when the click is outside the window frame and the window is visible.

`modules/widgets/components/mod.rs::init` is intentionally empty: there is no per-widget Rust code, no widget commands and no widget state on the native side.

## Sizing and positioning

The window is declared at `0×0`, origin `(0,0)`, `visible: false`, `focusable: false`, transparent, in `app/native/tauri.conf.json`. It is sized and placed by the frontend:

1. `useWidgets` reads `get_bar_window_frame` (Rust, `bar/window.rs`) once, cached under the `widgets-frame` query key.
2. On open it stores `{ name, rect }` and calls `window.show()`; the enter animation starts right after, unless a close already began.
3. A `ResizeObserver` on the content element (debounced ~4.17 ms) calls `setSize(content)`, then `setPosition`.
4. `windowX = clamp(rect.x, 0, bar.width - content.width) + bar.x`; `windowY = bar.y + bar.height + 4`. The `rect` comes from `getBoundingClientRect()` in the bar webview, so it is bar-relative CSS pixels.

There is no native positioning code and no shared state between the windows other than the Tauri events; a widget reads its own data through its own command (`get_battery_info`, `get_weather_config`, and so on).

## Invariants and pitfalls

- The window is not focusable, so "click outside" cannot be a blur handler. Closing depends entirely on the `CGEventTap`, which needs the same Accessibility permission as tiling; if `CGEventTapCreate` returns null the panel simply never closes on outside clicks (the error is logged).
- Click detection compares the `CGEvent` location in screen points against the window frame derived from `outer_position`/`outer_size` divided by `scale_factor`. Content that overflows the window frame reads as an outside click and closes the panel.
- Order matters: size before position, because the x-clamp uses the content width.
- `useWidgets` guards async `show`/`hide` with a generation counter (`closeGenerationRef`), because the exit animation delays `hide()` by `transition.duration` and a toggle can arrive mid-animation.
- `WindowFrame` in `Widgets.types.ts` also declares `screenWidth`/`screenHeight`; the Rust `WindowFrame` does not return them, so they are `undefined` and unused. Don't start relying on them.
- Widgets are `lazy()`-loaded in `Widgets.tsx`; adding one requires a `WidgetNames` entry, a render branch, and a trigger that calls `useWidgetToggle(name)`. See [recipes](../guides/recipes.md).

## Config

None. Widgets are always available; only the data behind them is configurable (`bar.weather` for the weather widget, see [config](../architecture/config.md)).

## Tests

`modules/widgets/window.rs` (`WindowFrame::contains` edges/corners, event and tap constants). UI: `widgets/Widgets.state.test.tsx` (open/close, race guard, clamping), `widgets/Widgets.test.tsx`, `widgets/components/{Calendar,Battery,Weather}/*.test.tsx`.
