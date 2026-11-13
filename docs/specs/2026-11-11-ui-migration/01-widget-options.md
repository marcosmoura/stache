---
status: done
---

# 01 — Widget options

## Scope

Make layout entries typed per widget, publish the options, and hand them to widgets. Rust (`stache-bar`, app bridge) and UI (`widgets/`). No widget is implemented here.

## Requirements

- An entry stays a widget name or a table. The table's keys are `widget` (required), `id` (optional) and that widget's options, flat:

  ```toml
  end = [
    "weather",
    { widget = "cpu", temperature_alert = 90 },
    { widget = "clock", show_seconds = false },
  ]
  ```

- Options in this spec:
  - `clock`: `show_date` (bool, default `true`), `show_seconds` (bool, default `true`), `hour_cycle` (`"h23"` or `"h12"`, default `"h23"`).
  - `cpu`: `temperature_alert` (integer degrees Celsius, 1 to 150, default `85`).
  - Every other widget has no options.
- A name-only entry and a table without option keys both mean the defaults.
- Decoding accepts the keys in any order, including options before `widget`.
- Errors keep exact field paths (`layout.end[1].temperature_alert`): an option of the wrong type, out of range, or an unknown enum value rejects the file. A key that is not `widget`, `id` or an option of that widget is an unknown key and warns, as before; this includes another widget's option.
- The JSON Schema describes an entry as a name or one object per widget, each with `widget` as a constant and only that widget's properties, so a form can be generated per widget. `id` stays a plain optional string. The template comment shows one entry with an option.
- `BarView.layout` items become a union tagged by `widget`: `{ id, widget, options }`, where `options` holds the resolved values (defaults applied) for that widget and is an empty object for widgets without options. The generated TypeScript is a discriminated union.
- An options change in an accepted reload publishes a newer `BarView`; it starts or stops no provider and does not recreate the panel.
- UI: `WidgetProps<K>` is `{ id, zone, options }` with `options` typed by kind. `WidgetSlot` passes the item's options. Changing only an item's options re-renders the widget and keeps its instance and state.
- Fix the leftover from the foundation review: in `windows/Bar/Bar.tsx` an exiting entry is cleared only by the zone it is exiting from, so an item that moved zones and is then removed does not blank mid-fade.

## Acceptance criteria

- [x] MIG-01-01: Decode, defaults, key order, every rejection with its field path and the unknown-key warning are covered for both widgets with options and one without.
- [x] MIG-01-02: The schema has one object variant per widget with a constant `widget`; the template round-trips; `bun run check:generated` passes.
- [x] MIG-01-03: An accepted options change publishes a greater sequence with the new resolved options; a rejected one publishes nothing.
- [x] MIG-01-04: A fixture widget receives typed options, re-renders on an options change and keeps its state.
- [x] MIG-01-05: The moved-then-removed item keeps its content until its own exit completes.
- [x] MIG-01-06: `docs/architecture/config.md`, `docs/features/bar.md` and `docs/architecture/ui.md` describe the entry options and how a widget declares options.
- [x] MIG-01-07: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Rust: colocated layout tests for each case above, catalog schema and template tests, publication on accepted and rejected reload.
- UI: `WidgetSlot` and `Bar` tests for options delivery and state survival; the exit regression with animations enabled.
- End-to-end: extend `bar-layout.spec.ts` with an options change emitted by the fake host.

## Notes

- A settings UI will edit the raw TOML: the published layout has resolved ids and options and does not say what was written explicitly. Nothing here needs to change that.
- Decision (orchestrator, during implementation): the catalog hands a decoder its keys in alphabetical order, so options always arrive before `widget`. The SDK gained `ModuleConfig::prune_section`, a hook that runs before decoding; Bar uses it to remove another widget's options from an entry and report them as unknown-key warnings.
