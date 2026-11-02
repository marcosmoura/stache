# Recipes

Read when you are making one of the common changes below and want the touch points without
reading the whole tree. Each step names the file that must change.

## New status bar widget

1. **Backend data.** Create `app/native/src/modules/bar/components/<name>.rs` and add
   `pub mod <name>;` to `modules/bar/components/mod.rs`. Expose either a
   `#[tauri::command]` returning a `#[serde(rename_all = "camelCase")]` struct (pull) or an
   event emitter (push).
2. **Long-lived watcher (optional).** If the widget needs a thread or event source, add
   `pub fn init(window: &WebviewWindow)` and call it from `components::init`. Only
   `keepawake` and `media` do this today.
3. **Register the command** in `lib.rs` inside `tauri::generate_handler![…]` — commands are
   ignored until registered.
4. **Event (push only).** Add the constant to `events.rs` and the matching string to
   `app/ui/types/tauri-events.ts`.
5. **Frontend.** Create `app/ui/renderer/bar/Status/<Name>/` with `index.ts`,
   `<Name>.tsx`, `<Name>.state.ts`, `<Name>.types.ts`, `<Name>.styles.ts` (if styled) and
   `<Name>.test.tsx`. See [react.md](react.md).
6. **Mount it.** Add the component to `app/ui/renderer/bar/Status/Status.tsx`; order in that
   file is left-to-right in the bar.
7. **Overlay (optional).** To open a widget popover on click: extend `WidgetNames` in
   `app/ui/renderer/widgets/Widgets.types.ts`, use `useWidgetToggle('<name>')` in the state
   hook, add `renderer/widgets/components/<Name>/`, and add a branch in `Widgets.tsx`.
8. **Verify.** `pnpm test:ui` and `pnpm lint`.

Remember the bar itself is off by default (`bar.enabled`).

## New CLI command

1. **Define the arguments** in `cli/commands/<domain>.rs` (or a new domain module) with the
   variant on the `#[derive(Subcommand)]` enum. Clap doc comments become `--help`; long
   examples go in `after_long_help`.
2. **Wire the domain** in `cli/commands/mod.rs`: add `pub mod <domain>;` if new, a
   `Commands::<Domain>(<Domain>Commands)` variant, and the match arm in `Cli::execute`.
3. **Implement** `pub fn execute(cmd: &<Domain>Commands) -> Result<(), StacheError>`. Use
   `cli/output.rs` (`print_highlighted_json`, `truncate`, `format_bool`) and a `--json`
   flag when the command prints state.
4. **Needs the running app?** Add a variant to `IpcCommand` in `platform/ipc_socket.rs`,
   handle it in `modules/bar/ipc_listener.rs::handle_command`, and send it with
   `ipc_socket::send_command`. Do not add a distributed notification — see
   [ipc.md](../architecture/ipc.md).
5. **Test the parse** next to the existing `Cli::try_parse_from` tests.
6. **Verify.** `cargo nextest run -p stache cli`.

## New config option

1. **Add the field** to the domain struct in `config/types/<domain>.rs` (or
   `config/types/root.rs`) with a user-facing doc comment, a `Default`, and an explicit
   `#[serde(rename = "…")]` when the key is not the field name.
2. **Read it** through `config::get_config()` or the clone taken at init — never by
   re-parsing the file.
3. **Template.** Update `config/template.rs::generate_config_template`; it is hardcoded.
4. **Sample.** Update `docs/samples/config.jsonc`.
5. **Schema.** Run `./scripts/generate-schema.sh` and commit `stache.schema.json`.
6. **Module gating.** If the option turns a module on or off, reflect it in that module's
   `LifecycleModule::status` so the tray shows `ConfiguredOff` instead of a dead toggle.
7. **Verify.** `cargo nextest run -p stache config` plus `pnpm lint`.

## New tiling layout

Not a single-file change; follow the chain so no `match` is left non-exhaustive.

1. `config/types/tiling.rs` — add the `LayoutType` variant (serde kebab-case) and its arm in
   `as_str`.
2. `modules/tiling/layout/<name>.rs` — add a `layout(…) -> LayoutResult` function over
   window ids, the usable frame, gaps and split ratios. Declare `mod <name>;` and add the
   arm in `layout/mod.rs::calculate_layout_full`.
3. `modules/tiling/actor/handlers/screen.rs` — map the config `LayoutType` to the runtime
   `LayoutType`.
4. `modules/tiling/actor/handlers/layout.rs` — add the variant to the `cycle_layout` match.
5. `modules/tiling/actor/handlers/resize.rs` — resize semantics and default split ratios.
6. `modules/tiling/actor/mod.rs` and `actor/minimum_size.rs` — minimum-size enforcement per
   layout.
7. `modules/tiling/borders.rs` — runtime `LayoutType` → `ConfigLayoutType`, and
   `config/types/borders.rs` if the layout gets its own border config.
8. `modules/tiling/commands.rs::layout_to_string_pub` and `cli/commands/types.rs::CliLayoutType`
   (+ its `as_str` and the `--layout` help text) so
   `stache tiling workspace --layout <name>` accepts it. The string must equal the serde
   kebab-case name.
9. Regenerate the schema and update `docs/samples/config.jsonc`.
10. Add tests where the neighbouring arms have them — `layout/mod.rs` covers routing and
    gaps, `handlers/resize.rs` covers ratio behavior.

## New Tauri event

1. **Rust.** Add `pub const NAME: &str = "stache://<module>/<name>";` inside the right
   `pub mod` in `app/native/src/events.rs`, then add it to **both** test arrays
   (`test_all_events_have_stache_prefix`, `test_event_naming_convention`).
2. **TypeScript.** Add the identical string to the matching `…Events` object in
   `app/ui/types/tauri-events.ts`.
3. **Emit.** `app_handle.emit(events::<module>::NAME, payload)` with camelCase JSON keys.
4. **Consume.** `useTauriEvent(name, cb)` for a one-off listener, or `useTauri({ eventName })`
   to refresh a query cache; declare the payload type in TypeScript.
5. **Verify.** `pnpm test` — the Rust assertions catch a name that breaks the convention.
