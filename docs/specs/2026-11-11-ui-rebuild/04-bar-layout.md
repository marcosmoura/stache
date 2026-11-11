---
status: draft
---

# 04 — Bar layout mechanism

## Scope

`[layout]` in `bar.toml`, the normalized layout in `BarView`, the widget contract and registry, the layout engine and the Bar window. Depends on parts 02 and 03. No widget is implemented.

## Requirements

### Configuration (Rust, `stache-bar`)

- `BarConfig` gains `layout` with three ordered zones, `start`, `center` and `end`:

  ```toml
  [layout]
  start = ["spaces"]
  center = ["media"]
  end = ["weather", "cpu", "battery", "keep_awake", "wifi", { widget = "clock", id = "clock" }]
  ```

- An entry is a widget name, or a table with `widget` (required) and `id` (optional). The table form is where each migrated widget later adds its own typed options.
- Widget names are a closed snake_case enum, `BarWidgetKind`: `spaces`, `media`, `weather`, `cpu`, `battery`, `keep_awake`, `wifi`, `clock`.
- Defaults: a missing `[layout]` is the arrangement above (today's order). A missing zone inside a present `[layout]` is empty. An empty zone is valid, and so is a layout with no entries.
- Normalization gives every entry an `id`. An entry without one gets its widget name, then `<name>_2`, `<name>_3` in file order across all zones, skipping any explicit id.
- The same widget may appear more than once.
- Validation rejects, with field paths such as `layout.end[2].widget`: an unknown widget name, an empty or duplicate explicit `id`, an `id` that is not ASCII letters, digits, `-` or `_`, and a wrong-typed entry. A rejected file keeps the last accepted layout, like every other rejected reload.
- Unknown keys inside an entry table warn and do not reject, like unknown keys elsewhere.
- Schema, template (with a commented example), migration (a migrated legacy file gets no `[layout]` and therefore the default) and the catalog spelling test cover the new keys.

### Publication

- `BarView` gains `layout: { start, center, end }`, each a list of `{ id, widget }`, from the accepted configuration. It changes only with an accepted reload and is delivered through the existing snapshot and `bar-changed` event with the usual sequence.
- A layout change does not start or stop any provider and does not recreate the panel.

### Widget contract and registry (`src/ui/src/widgets/`)

- `WidgetProps`: `{ id, zone }` now; per-widget options join it during migration.
- A widget definition is `{ load }`, a function returning a dynamic import of the widget component, so each widget is its own chunk and a widget absent from the layout is never downloaded.
- The registry maps `BarWidgetKind` to a definition. In this spec the production registry is empty and typed as partial. The migration spec fills it and makes it exhaustive, so a kind added in Rust then fails type checking until it is registered.
- The registry reaches the layout engine through a provider with the production registry as default, so tests and the end-to-end harness can supply fixture widgets.

### Layout engine and Bar window

- `windows/Bar/` reads the Bar snapshot through `barFeed` and selects `layout`, `height` and `padding`.
- It renders three zone containers (start aligned to the leading edge, center centered on the Bar regardless of the other zones' widths, end aligned to the trailing edge) and, per item, a `WidgetSlot` keyed by `id`.
- `WidgetSlot` resolves the definition and renders the lazy widget inside its own `Suspense` and error boundary: one widget that throws or fails to load shows nothing in its slot and leaves the others working. The failure is logged once.
- A kind with no registered definition renders nothing and leaves no empty element. In development it warns once per kind.
- Reordering, adding and removing items animate with Motion layout and presence animations using the part 02 presets, and are instant under reduced motion. An item whose `id` survives a layout change keeps its component instance and state, including when it moves to another zone.
- For an `unavailable` or `unauthorized` snapshot, or before the first snapshot, the window renders the zone containers with no items and no error UI.
- `router.ts` maps `bar` to this window. `shell`, `test` and `access` stay on the placeholder.
- The Bar does not implement the slide-away on menu visibility, hover behavior or any widget; those belong to the migration spec.

## Acceptance criteria

- [ ] UI-04-01: Decode, defaults, normalization and every rejection listed above are covered, with field paths; schema, template, bindings and migration agree; `bun run check:generated` passes.
- [ ] UI-04-02: An accepted layout reload publishes a `BarView` with the new layout and a greater sequence; a rejected reload publishes no layout change.
- [ ] UI-04-03: With fixture widgets registered, the Bar renders each zone's items in configured order, and the center zone stays centered when the start and end zones have different widths.
- [ ] UI-04-04: After a layout change, an item with a surviving `id` keeps its state in its new position; removed items unmount; added items mount.
- [ ] UI-04-05: A widget that throws while rendering and a widget whose `load` rejects each leave an empty slot and do not affect the other items.
- [ ] UI-04-06: With the production registry, the default layout renders three empty zones, no error and no widget chunk request.
- [ ] UI-04-07: The repeated-widget case renders both instances with distinct ids.
- [ ] UI-04-08: `docs/features/bar.md`, `docs/architecture/config.md`, `docs/architecture/ui.md` and `docs/roadmap.md` describe the layout keys, the widget contract, how to add a widget, and that the Bar has no widgets until the migration.
- [ ] UI-04-09: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Rust, colocated in `stache-bar` and the app catalog tests: string and table entries, mixed zones, missing section, missing zone, empty zones, id assignment with repeats and explicit collisions, each rejection, unknown entry key warning, template and migration output, publication on accepted and rejected reload.
- `Bar.test.tsx` and `WidgetSlot.test.tsx` with fixture widgets: order, centering, state survival across reorder and zone move, throw isolation, load failure isolation, unregistered kind, all three snapshot outcomes.
- End-to-end scenarios (`bar-layout.spec.ts`), driven only through the fake host as the real host would:
  1. Boot the harness page with fixture widgets and a three-zone snapshot; assert visible order and positions.
  2. Emit `bar-changed` with a reordered layout, then one that removes and adds items; assert the DOM follows and a stateful fixture widget keeps its state.
  3. Emit an older sequence after a newer one; assert nothing changes.
  4. Start `unavailable`, then become `available`; assert the items appear.
  5. Production `index.html?surface=bar` with the default layout in light and dark: three empty zones, token colors correct, zero widget chunks requested, no console errors.

## Notes

- The harness page (`e2e/harness.html` with its entry) boots the same `App` with a fixture registry. It is served only by the dev server and is not part of the production build.
- Stopping the provider of a widget that is not in the layout is a later optimization, outside this spec.
- A native run that edits `bar.toml` and watches the live Bar needs the user's go-ahead right before launch; without it the reload path is fake-proven only.
