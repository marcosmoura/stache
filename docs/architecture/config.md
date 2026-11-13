# Configuration

Read when changing configuration files, reload behavior, schema generation,
migration or the `config`/`schema` commands.

Configuration is file-backed and typed. `stache-sdk` owns the parser, catalog,
store and watcher; the application composition registers the concrete types;
`stache-cli` implements the local commands without importing app or module
crates.

## Key spelling

Every declared key and every unit-enum value is snake_case (`exec_on_startup`,
`hold_duration`, `default_layout`, `preset_on_open`, `split_vertical`, `ease_out`,
`visual_crossing`, `starts_with`, `right_click`). There are no serde aliases: an
old camelCase or kebab-case key is an unknown key (a warning) and an old enum
value is a type error that names its field. User data is never renamed: keybinding
shortcuts, command strings, bundle ids, paths, colors, device names and
percentage strings such as `"50%"`. A catalog-level test (`config_catalog.rs`)
checks every property name and string enum value in every registered schema over
the composed app catalog, so a new type that forgets `rename_all = "snake_case"`
fails. `LayoutType` uses the same spelling on the socket and in query results;
only `stache tiling --layout` also accepts `split-vertical`/`split-horizontal`.

## Files and paths

The directory is, in order: an explicit `--config DIR`, a non-empty
`$XDG_CONFIG_HOME/stache`, then `~/.config/stache`. `-c DIR` and
`--config=DIR` are accepted before or after a subcommand. `init`, `path` and
`schema` never require the directory to exist.

Each file is its own catalog document, named after the exact identifier:

```text
~/.config/stache/
  global.toml       GlobalConfig
  bar.toml          BarConfig
  commandQuit.toml  CommandQuitConfig
  keepAwake.toml    KeepAwakeConfig
  keybindings.toml  KeybindingsConfig
  menuAnywhere.toml MenuAnywhereConfig
  notunes.toml      NoTunesConfig
  proxyAudio.toml   ProxyAudioConfig
  tiling.toml       TilingConfig
  wallpapers.toml   WallpapersConfig
```

The loader reads only `global.toml` and registered IDs; there is no JSONC or JSON
fallback. Legacy `config.jsonc`/`config.json` locations are read only by
`config migrate`, which is also the only code that still parses JSONC
(`jsonc-parser` is a `stache-cli` dependency only, not an SDK one).

Every file starts with `#:schema <url>`, the Taplo/Tombi convention that points
an editor at `schemas/<id>.schema.json`. It is an ordinary comment to the parser;
there is no `$schema` data key.

`global.toml` holds `logging`, `exec_on_startup` and `[ui]`. `[ui] appearance`
is `"system"` (default), `"light"` or `"dark"`; any other value is a type error at
`ui.appearance`. The effect is app-wide: the host sets the application
appearance (`AppHandle::set_theme`, i.e. `NSApp.appearance` = nil, Aqua or Dark
Aqua), and every window and the Bar `NSPanel` set none of their own and inherit
it. In tao a per-window theme also sets the application appearance, and a build
with no theme does not reset it, so no window is built with a theme. The webview's `prefers-color-scheme` follows that native appearance,
so the UI's `light-dark()` tokens pick Latte or Mocha with no script (see
[ui.md](ui.md#tokens-and-themes)). No window is forced dark.

```toml
[ui]
appearance = "system" # "light" | "dark"
```

`bar.toml` is registered for configuration, schema, template and migration
only (`enabled` default `false`, `height` 28, `padding` 12, and `weather` with
`provider` `auto`|`visual_crossing`|`open_meteo`, `api_keys`, `default_location` and
`precise_location` (default `false`) and `refresh_interval` (integer minutes, default 20, accepted 5 to 1440; out-of-range or wrong-typed values are rejected at `weather.refresh_interval`; migration writes 20).
The Bar runtime, panel and data commands are documented in [bar.md](../features/bar.md);
this page covers only the configuration contract. Dimensions keep the legacy `u16`
range and an explicit `0` is preserved, never replaced or clamped; feasibility
is a later runtime concern. `WeatherConfig::api_keys_path` trims `api_keys`,
expands injected `~`/`~/`, resolves anything else under the configuration
directory and never expands shell or environment syntax or uses the process
working directory. The Bar weather path reads `VISUAL_CROSSING_API_KEY` from that file on each
`bar_weather` call (no watcher). See [bar.md](../features/bar.md).

`bar.toml` `[layout]` (`BarLayoutConfig`, `src/crates/modules/bar/src/layout.rs`)
has three ordered zones, `start`, `center` and `end`. An entry is a widget name
(the closed snake_case enum `BarWidgetKind`: `spaces`, `media`, `weather`,
`cpu`, `battery`, `keep_awake`, `wifi`, `clock`) or a table with `widget`
(required), an optional `id` and that widget's options as flat keys
(`{ widget = "clock", show_seconds = false }`). Options: `clock` has
`show_date` and `show_seconds` (booleans, default `true`) and `hour_cycle`
(`"h23"` or `"h12"`, default `"h23"`); `cpu` has `temperature_alert` (integer
degrees Celsius, 1 to 150, default 85); the other widgets have none. A name and
a table without option keys both mean the defaults. A missing `[layout]`
is the default `start = ["spaces"]`, `center = ["media"]`,
`end = ["weather", "cpu", "battery", "keep_awake", "wifi", "clock"]`; a zone
missing from a present `[layout]` is empty.

The "string or table" shape is a hand-written serde visitor, not an untagged or
internally tagged derive (both buffer the value and lose paths), so type errors
keep their exact path (`layout.end[2]`, `layout.end[2].widget`,
`layout.start[0].id`, `layout.end[1].temperature_alert`): an option of the wrong
type, out of range or with an unknown enum spelling rejects the file. Keys may
come in any order. The decoder reads a table's keys alphabetically (the JSON
map), so options arrive before `widget`; each option name belongs to exactly
one widget and is decoded in place for its owner. Another widget's option on an
entry is an unknown key: `ModuleConfig::prune_section`, a catalog hook run
before every decode (default no-op), removes it from the JSON value and reports
its path (`layout.end.1.show_seconds`) with the unknown-key warnings; other
unknown keys reach `serde_ignored` as usual. The config type is
`BarLayoutEntryTable`, internally tagged by `widget` with one variant per
widget over `BarEntryFields<Options>` (`id` plus the flattened options;
`NoOptions` for a widget without options). The schema describes an entry as an
`anyOf` of a widget name and a `oneOf` with one object per widget (`widget` a
`const`, only that widget's properties with their defaults and ranges; `id` a
plain optional string with `minLength` 1 and the pattern `^[A-Za-z0-9_-]+$`).
`BarConfig::validate` rejects a duplicate explicit id, naming both paths, and a
`temperature_alert` out of range in a configuration built in code.
`BarLayoutConfig::normalized()` assigns ids (an explicit id is kept; otherwise
the widget name, then `<name>_2`, `<name>_3`, ... in file order across all
zones, skipping explicit ids), resolves options and is what the host publishes
as `BarView.layout`: items `BarLayoutItem`, tagged by `widget`, of
`{ id, widget, options }` with every option resolved (`{}` for a widget without
options); the generated TypeScript is a discriminated union on `widget`. The
template shows the default under `[layout]` with a commented example that
includes an option; migration writes no `[layout]`, so a migrated file uses
the default.

`notunes.toml` is registered for configuration, schema, template and migration,
and backs the shipped blocker runtime: enabling it installs application
observation and terminates Apple Music/iTunes. Catalog support alone starts no
module, creates no webview and installs no native observation; see
[notunes.md](../features/notunes.md).

`keybindings.toml` is registered for configuration, schema, template and
migration and drives the Keybindings module (see [keybindings.md](../features/keybindings.md)).
An accepted reload is observed by the module through its accepted-config watch: a rejected or
unchanged file changes nothing and cancels no running command, an accepted changed file
invalidates the old dispatcher generation. `KeybindingsConfig`
has `enabled` (default `false`) and `bindings` (default `{}`), a map from a
shortcut string to one command string or an ordered list of strings (the same
wire shape as `exec_on_startup`). Validation is shape-only: non-string values and
array elements are rejected with field paths, while a malformed shortcut spelling
is not a file rejection (planning diagnoses it per entry in the module's runtime status). Migration
wraps the whole legacy map as `bindings` and sets `enabled` from map
nonemptiness, even when every command is blank; a legacy shortcut literally named
`enabled` or `bindings` is a binding, never the new envelope. A missing legacy
section emits the disabled template. See [keybindings.md](../features/keybindings.md).

`tiling.toml` is registered for configuration, schema, template and migration,
and the Tiling module itself is registered **default disabled** (`enabled` is
`false`): the `lib.rs` composition appends `TilingFactory::production` after
Wallpapers, Keybindings and Hold-to-Quit. Construction and registration are inert
(no observer, worker, thread or native effect); only `enabled = true` starts the
core and workspace runtime. `animations.enabled` starts one owned animation worker
(see the Tiling feature doc). `borders.enabled = true` turns on borders that
Stache draws itself around Tiling-managed windows; nothing external is started,
changed or left behind (see [Tiling borders](../features/tiling.md#borders)).
The `enabled` schema description (generated from the config doc comment) says
so. Disabled (the default) creates nothing native. `TilingConfig` (crate `stache-tiling`) keeps
every legacy shape: `enabled` (`false`), `default_layout` (one authoritative
`dwindle` default; `dwindle|split|split_vertical|split_horizontal|monocle|master|grid|floating`),
`workspaces` (`name`, optional `layout`, `screen` default `main`, `rules`, and the
inert exact-spelled `preset_on_open`), `ignore` rules (`app_id`, `title`,
`app_name`; an empty rule is valid and matches nothing), `animations`
(`{enabled:false,duration:200,easing:"ease_out"}`; six easings), `gaps`
(uniform, per-axis or per-side values, global or a per-display list),
`floating` (`default_position` `center|default`, `presets` with pixel or `"50%"`
dimensions, `x`/`y`/`center`), `master` (`ratio` 60, `position` `auto`) and
`borders` (per-state `false`, solid, gradient with optional animation, or glow;
per-layout overrides; `hidpi` (the removed `style` key is an unknown-key warning; borders are always round); `ignore`; and, read by
the border engine, `transition` `{duration, easing = "ease_out"}`,
`order` `below|above` and the gradient `animation.style`
`ping_pong|once|loop`, all optional with the legacy meaning when omitted). User shapes
round-trip unchanged. Validation rejects duplicate workspace names
(exact case, matching workspace lookup) or preset names (case-insensitive), a `preset_on_open` naming no preset, `master.ratio` above 100,
zero, negative, non-numeric or non-finite preset dimensions, gap sums that
overflow, malformed colors, unsupported `order` or `animation.style`
and a bare `true` border state,
with field paths; a rejected file leaves the module inactive and never falls back
to enabled defaults. The grid cap of ten and the applied floating
`default_position` now apply to the core runtime; animation, drag and
borders apply to the runtime; native tabs follow the shared tab bar (fake-proven, Finder-probed). See [tiling.md](../features/tiling.md).
Bar reads no Tiling configuration and Tiling reads no Bar configuration: the Bar strip
comes from Tiling's published state and Tiling's top inset from the host screen
reservation ([Bar](../features/bar.md#tiling-strip-data)).

`menuAnywhere.toml` backs the MenuAnywhere runtime: the host registers it
through `MenuAnywhereFactory`, so the file is bound, watched and reloaded like
any module file. Registration is inert (including an enabled file); only an
accepted `enabled = true` with Accessibility granted starts the module, and the
runtime is documented as implemented but with native behavior unverified.
`MenuAnywhereConfig` has `enabled` (default `false`), `modifiers` (default
`["control", "command"]`; `control|option|command|shift`) and `mouse_button`
(default `"right_click"`; `right_click|middle_click`); `{}` uses the per-field
defaults, unknown keys warn, and unknown enum spellings, wrong types, `null`
fields and non-object roots are rejected with field paths. Empty `modifiers`
(`[]`) is valid and captures unmodified matching clicks system-wide while
enabled; the schema description and template comment
say so. Duplicates are valid, preserved in the file and count once. The crate
also provides a pure four-bit `ModifierMask` whose equality matching ignores
unrelated flags such as Caps Lock. `middle_click` means button number 2 only, so
back and forward buttons never match. See
[menu-anywhere.md](../features/menu-anywhere.md).

`keepAwake.toml` is registered for configuration, schema, template and
migration, with only `enabled` (default `false`). It backs the shipped Keep
Awake runtime: an accepted `enabled = true` starts one module generation that
manages the lock-aware power assertion, while `false` stops it. Construction,
registration and configuration acquire nothing. A rejected parse, type, I/O or
deleted file keeps the last accepted revision and live lease and records a
diagnostic; identical last-good bytes clear it without lease churn. See
[keep-awake.md](../features/keep-awake.md).

`wallpapers.toml` is registered for configuration, schema, template and
migration but has no runtime in this release: nothing binds it, so the watcher
and `reload_all` skip it and `core.status` shows it valid but not active.
`WallpapersConfig::validate` is structural only. Source resolution is a separate
shared API that resolves the trimmed `path` or `list` against the configuration
directory and injected home, and an explicit CLI file against the invocation
CWD. See [wallpapers.md](../features/wallpapers.md).

`commandQuit.toml` is bound to the [Hold-to-Quit](../features/hold-to-quit.md)
module (`CommandQuitConfig`, default-disabled). Its keys are `enabled`
(default `false`), `hold_duration` (milliseconds, default `1500`) and
`dismiss_delay` (milliseconds the HUD stays after an early release, default
`1500`, `0` hides at once), `position` (`"bottom"` default, `"top"`, `"left"`,
`"right"`, `"center"`: where the HUD appears) and `animation` (`"fade"` default
or `"slide"`: how it enters and leaves; Reduce Motion makes a slide a fade).
`position` and `animation` are lowercase string enums exported to the generated
TypeScript; an unknown string or wrong type is a type error naming the key.
Both durations are `u64` in Rust but TOML integers are `i64`: every value from
`0` to `9223372036854775807` is valid, a larger value is a type error naming the
key (reachable from migration JSON; TOML text above `i64::MAX` is already a parse
error) and `validate` rejects it for a directly constructed value. The JSON
Schema declares `maximum: 9223372036854775807`. Wrong types are type errors
naming the key. Because TypeScript `number`
cannot represent every `u64`, both are excluded from the generated TypeScript
(`#[specta(skip)]`); a consumer that needs one must approve an explicit
representation first. A migrated legacy file has none of `dismiss_delay`,
`position` or `animation` and decodes to the defaults (`1500`, `bottom`, `fade`). A reload to a different
value cancels any in-flight hold and arms new presses only after the matching
accepted publication. See [hold-to-quit.md](../features/hold-to-quit.md).

## Parsing

`toml` parses the text as TOML 1.0; the root is always a table. The table is
converted to a JSON value so the catalog keeps one type-erased decode boundary
(`serde_path_to_error` for field paths, `serde_ignored` for unknown keys).
Parse errors carry a 1-based line and character column computed from the byte
offset of the parser's error span, and the message is the parser's single-line
reason, not its multi-line snippet. Duplicate keys, redefined tables, invalid
escapes and unterminated strings are parse errors. TOML date/time values and
`nan`/`inf` floats are type errors naming the field (no configuration field
accepts them). Any key the target type does not declare, including a
`$schema` key, is ignored and reported as a non-fatal warning, so a typo never
invalidates the file. A whitespace-only file is invalid with the message
``file is empty; add a `#` comment or a key to use defaults``, so a truncated
save never resets a module; a file that holds only comments (such as just the
`#:schema` line) is an empty table and uses the defaults.

A missing first-load file uses the type's `Default`. A supplied file that
fails to parse, deserialize or validate is never replaced by defaults: it is
inactive and carries a diagnostic. Empty objects keep per-field defaults.

## Catalog and store

`ConfigCatalog::register::<C>(id)` stores the type-erased default, decoder,
validator and schema generator. `ConfigStore` binds a typed
`ModuleHandle<C>` to its catalog entry and feeds accepted candidates to the
existing `Registry`; it owns no second lifecycle.

`ConfigStore::initial::<C>(id)` returns `Default`, `Loaded` or `Invalid`.
An `Invalid` first file registers the module through
`Registry::register_rejected` (revision `0`, `Stopped`, diagnostic); a later
valid save commits revision `1` and starts the module (once the registry has
started) without a restart.
`reload_one`, `reload_global` and `reload_all` return applied/unchanged/rejected
outcomes; `apply(ChangeSet)` maps a watcher batch to those reloads. Only
accepted text is cached, so a rejected candidate is retried when the same
content is saved again. A rejected or deleted file keeps the last good registry
revision, status and resources and records a separate diagnostic. `ConfigStore`
never executes `exec_on_startup`.

## Edits

The settings editor and the tray's **Enable** action share one comment-preserving
edit (`stache_sdk::config::edit_ops`); nothing reserializes a file. An edit is a
`ConfigEditOp` at a key path (`ConfigPath`, a list of table keys and array
indices, so quoted keys such as `"CapsLock+a"` need no escaping):

- `set { path, value }` sets or inserts a JSON value. A value already in the file
  has only its value span replaced (its trailing comment stays); an existing
  standard table or array of tables is set member by member. `null` is never
  written: `set` with `null` is `remove`.
- `remove { path }` removes a key (reset to default: the whole `key = value`
  line) or an array element; removing a table or array-of-tables element removes
  its header, body and sub-tables. Removing a missing key is unchanged.
- `insert { path, index, value }` inserts an element into an inline array (in its
  one-line or multi-line style) or a new `[[table]]` element.
- `move { path, from, to }` reorders array elements; each array-of-tables
  element moves with its header, body, sub-tables and the comment lines directly
  above it, and each element of a one-element-per-line array moves with its
  comments.
- `rename { path, from, to }` renames a map key (re-rendering the headers of a
  renamed table); it is refused when `to` exists.

Only the touched span changes; the `#:schema` line, every other comment, key
order, CRLF line endings and a missing trailing newline stay byte for byte. A new
key goes after the last key of its table, before any sub-table header, in the
file's line ending; in a file with no root keys a root key goes after the leading
comment lines, so `#:schema` stays on line 1. An object (or a list of objects)
set under the root or a standard table outside an array of tables becomes a
`[table]` (or `[[table]]`) block appended at the end of the file; elsewhere
values are written inline. A key under a dotted table is written as a dotted key
in its nearest explicit table. Every planned text is re-parsed and compared with
the same operation applied to the file's JSON value; a mismatch is refused and
nothing is written. Path errors (a segment that crosses a non-table, an
out-of-range index, an existing rename target, a value TOML cannot hold) are
typed refusals.

**Hash and stale.** A document's hash is FNV-1a 64-bit over the exact bytes as
16 lowercase hex digits (`content_hash`; `None` for a missing file). It detects
change, not tampering. Every editor edit carries the `baseHash` it was based on;
`plan_edit` reads the file once and refuses with `stale` (writing nothing) when
the bytes hash differently.

**Validation.** `plan_edit` refuses a symlink, an unreadable file and a currently
invalid file (an invalid file is read-only until a valid save lands). A missing
file starts from the generated template. The candidate text is decoded and
validated through the catalog exactly as a reload would before any write; a
failure returns the catalog's message, line/column and field path (falling back
to the edited path when the catalog names none). `set_enabled` (the tray) is
`set` on the root `enabled` after refusing a non-boolean `enabled` (including a
`[enabled]` table).

**Commit.** The commit prepares and syncs a same-directory temporary file first,
then re-reads the original bytes as close to the rename as possible; a change in
between is reported as `stale`. A missing target uses a no-clobber rename, so a
concurrently created file is never replaced. A non-cooperating writer can still
change the file between that last check and the rename; compare-plus-rename is
not an atomic compare-and-swap on POSIX. Refusing a configuration file that is
itself a symlink is deliberate: an edit never replaces the link. A replacement
keeps the original file's permission bits, but the rename creates a new inode,
so hard links to the old file are not updated.

After a successful commit the host calls `ConfigStore::reload_one` (or
`reload_global` for `global.toml`) once, on the host runner, and returns the new
hash and the reload outcome (`applied` with the revision, `unchanged`, or
`rejected` with a diagnostic). The watcher later reports the same bytes as
unchanged, so a module is not reconfigured twice. An unchanged enabled value
still syncs the disk state and retries an eligible failed start instead of only
clearing the error. **Pause/Resume** never edit a file; a disabled module cannot
be resumed from the tray. File diagnostics stay visible separately from runtime
status.

## Document reads and change events

`config_document(file)` reads one catalog file once, without touching a module
lock: file name and path, state (`missing`, `valid`, `invalid`, `symlink`), hash,
the parsed value (only the keys present in the file; `null` when missing or
invalid), the catalog default, the JSON Schema, the diagnostic (message,
line/column, field) and unknown-key warnings. A symlinked file is read through
the link but is read-only. `config_files()` returns the last published
`HostStatus.config`. `open_config_file(file)` creates a missing file from the
template (no-clobber) and opens it with the default app (`open`). Files are named
only by catalog file name; nothing accepts a path.

The host emits `config-changed { files }` (catalog file names) after every
committed edit (editor or tray) and after every watcher batch that touched
catalog files, whatever the reload outcome (accepted, unchanged, rejected or
deleted; a rescan names every file). It is emitted from the host runner after
the reload and the status publication, holding no store lock. A watcher batch
that only echoes the host's own committed bytes (same hash, not rejected) is not
announced a second time, so one edit is one event.

## Reloading

`ConfigWatcher` watches the configuration directory itself; while that directory
is absent it watches the nearest existing ancestor and moves the watch into the
directory once it appears. It debounces for 200 ms, watches directories rather
than files and therefore handles atomic save-by-rename. It reports only the
catalog files in the batch. Identical source bytes are a no-op only while no
diagnostic is held; a held diagnostic re-decodes and reapplies the file, so
undoing a bad edit or recreating a deleted file clears the diagnostic, and an
identical save retries a failed start. A changed file that decodes to the same
typed value is also unchanged. Only the affected module reconfigures.
Once the host has called `start_all`, an accepted reload that enables a stopped
or disabled module starts it, and a disabling reload stops it; a paused module
stays paused. If that start fails, the configuration is still committed, the
module is `Failed`, and the store reports `Applied` plus an application
diagnostic; the next changed configuration retries. Deleting a file retains the
last good runtime until a valid replacement is written.

`global.toml` updates the injected `LogFilter` immediately, so level changes
are live. Every accepted `global.toml` (the first load, including the
default for a missing file, and every applied reload) reaches the store's
`on_global_accepted` observers; the host's observer calls
`WindowBackend::set_appearance`. The first accepted value and every change
queue one main-thread job that applies it to the application
(`WindowHost::apply_appearance`), whether or not a window is live; live windows
and panels follow at once and later builds inherit it, nothing is recreated, and
`"system"` clears the override so the system appearance is followed again.
A job superseded by a newer value does nothing. A rejected or unchanged reload calls no observer and changes nothing. `exec_on_startup` is read once by the desktop host, never by config
loading, reload or CLI. The host starts it once after initial setup (after
registry start, socket and watcher) on its own owned thread, sequentially with
legacy quoting rules, stopping at the first failure; shutdown cancels it, kills
only the child it spawned directly and does not start later commands. A child not
confirmed gone within 2 s is retained by the runner with a `stache: startup:`
stderr diagnostic instead of being waited for indefinitely. The exact bare name
`stache` runs the running Stache executable (re-entered with the hidden
`--stache-internal-cli` flag), never a `stache` found on `PATH` or the legacy app.
Readiness, tray actions and Quit never wait for a long-lived command. A second
instance, CLI invocation and every reload skip it.

## Accepted configuration

Every registered module has one accepted-configuration cell separate from its
lifecycle state. `Registry::accepted_watch(handle)` returns a typed
`AcceptedWatch<C>`; its configuration type is fixed by the handle and checked
against the registered type, so a handle from another registry with the same
identifier fails with `RegistryError::ConfigTypeMismatch`.

`read()` returns the last `AcceptedConfig { revision, config }` pair or `None`.
Both fields come from one lock and the accessor takes no module, lifecycle or
reload lock and never reads disk, so a reader cannot observe a mixed pair or
wait for module work. `wait_for_change(seen_revision, timeout, token)` returns
`Changed`, `TimedOut`, `Cancelled` or `Closed`; an absent configuration is
revision zero. The check and sleep share the cell lock, so publication between
them cannot be lost. Registry invalidation, including shutdown and teardown,
closes the cell and wakes waiters while leaving the last pair readable.

Registration publishes a validated configuration at revision `1`; a never
existing default-valid file therefore reads as accepted defaults. Invalid,
rejected or malformed initial configuration publishes nothing. A reload
publishes immediately after the registry assigns `state.config` and increments
the revision, before a disabling quiesce or start attempt, so a committed
configuration stays visible even when a later start or stop fails. Rejected
validation, a failed reconfigure, a failed pre-commit quiesce, deleted files and
unchanged reloads publish nothing. The old pair stays readable throughout
`Module::reconfigure`.

`FactoryContext::accepted_watch` exposes the same handle to factory socket
handlers without exposing the mutable registry or store. See
[sdk.md](sdk.md#accepted-configuration).

## Diagnostics

`ConfigDiagnostic` carries the file ID, path, category (I/O, parse, type,
validation, deleted, application), optional line/column for parser errors and
the serde field path for type errors. The registry keeps its own validation
diagnostic on the module snapshot; the store keeps the file-level one. They
describe the same candidate and never change the observed status. Keep Awake's
`keepAwake.status` overlays the held store diagnostic before the registry
diagnostic; see [ipc.md](ipc.md#keep-awake-status).

Unknown keys are warnings, not diagnostics. The store keeps a separate warning
list per file and `config path` renders it, so the file stays accepted and an
unchanged save is still a no-op instead of re-decoding forever.

## Commands

```sh
stache config init [-f] [-p DIR] [--stdout]
stache config path
stache config migrate [--from PATH] [-f]
stache schema [global|bar|commandQuit|keepAwake|keybindings|menuAnywhere|notunes|proxyAudio|tiling|wallpapers]
```

The commands run from the single `stache` binary. From a checkout use
`cargo run -q -p stache-app --bin stache -- …`; the packaged `.app` always
launches the desktop, so command-line use requires a standalone binary. None of
these commands starts the tray, a webview, the socket server or a module.

`config path` also contacts a running host when one is reachable and its
configuration directory matches the selected directory; that host's status
marks files `(exists, active)` only when their current bytes were accepted for
the loaded global or a bound module. Unloaded, unbound and newly edited files
are not active. Held host diagnostics survive valid/deleted disk content and
are printed on stderr (`stache: …`); path/marker output stays on stdout.
Offline runs stay `(exists, offline)` and never fail because the host is absent.

`init` writes every registered template (global first, then ID order) with
atomic per-file writes. It preflights all collisions and changes nothing
without `--force`; `--force` replaces only planned files. A replaced file keeps
its permission bits; a new file keeps the private temporary-file mode. Atomic
replacement breaks hard links. `--stdout` writes
nothing and prints `# file: <name>` blocks. `-p` wins over `--config`, which
wins over the default.

## Path command markers

`path` prints the directory and expected files. Existing valid files are
`(exists, offline)` in a standalone run; a reachable host may mark them
`(exists, active)`. An invalid file is `(exists, invalid)` and prints its
diagnostic. Unknown-key warnings print under their file without changing its
marker. Missing files have no marker. Offline means the local file validates,
not that a host accepted it.

A registered `<id>.jsonc` with no `<id>.toml` next to it is stale: the loader
ignores it, and a trailing line ``note: <id>.jsonc is ignored; run `stache config
migrate` `` follows the entries (one per stale file, also when a host answers).
Exit status and markers do not change. The desktop host logs the same hint once
at startup, as a single `warn` record that lists every stale file; reloads and
watcher batches do not repeat it. Once both files exist there is no hint.

A separate legacy-config note appears when a legacy source exists (the first
existing path of `ConfigPaths::legacy_sources`, as migrate discovers it) and
`global.toml` does not: one line ``note: <source> is a legacy config and is not
read; run `stache config migrate` `` after the entries and stale hints. When a
registered `<id>.jsonc` also sits in the directory, plain migrate would pick
those, so the line names `stache config migrate --from <source>` instead. The
desktop host logs the same line once at startup as an `info` record
(`log_legacy_config_hint`). Both use `ConfigPaths::legacy_config_hint`, which
checks paths only: it never opens, parses, copies or rewrites the source and
prompts nothing.

Trust warnings (see [Trust model](#trust-model)) follow the stale-JSONC notes and the legacy-config note,
one line per flagged path, in both the offline and the host-answered output.
They change no marker and no exit status.

`config path` and the `schema` output never write ANSI when piped. `schema`
styles keys, strings, numbers and literals only when stdout is a terminal; see
[ipc.md](ipc.md#output).

`schema` defaults to the global schema; `schema bar`, `schema commandQuit`, `schema keepAwake`, `schema menuAnywhere`,
`schema proxyAudio`,
`schema notunes`, `schema tiling` and `schema wallpapers` print those registered module schemas.
Unknown IDs fail.

## Trust model

Configuration files are code. `global.toml` `exec_on_startup` and
`keybindings.toml` bindings run commands as children of the
Accessibility-trusted app, and keybindings hot-reload, so anyone who can write
these files runs commands as the user. Every process of the same user is
trusted; the boundary is other accounts.

The loader follows symlinks and never refuses a file because of its owner or
mode. [`inspect_trust`](../../src/crates/sdk/src/config/trust.rs) only warns. It
examines, following symlinks:

- the configuration directory;
- every registered file that exists (`global.toml` included);
- for a file that is itself a symlink, the directory holding its target.

A path is flagged when its owner is neither the effective user nor root, or its
mode is group-writable (`0o020`) or world-writable (`0o002`). Group write is
flagged even though macOS users share `staff`. A symlink target's directory with
the sticky bit (`0o1000`, as in a Nix store or `/Users/Shared`) is not flagged
for group or world write, since others cannot replace or remove the file there;
its owner is still checked, and the configuration directory is flagged even when
sticky because someone else could create a missing file in it. ACLs are not
inspected: only owner and mode bits are. Missing files and metadata errors
are skipped; the loader reports I/O problems. Ancestors are not walked. Each
path is reported once, by its canonical spelling, so a symlinked
`~/.config/stache` names the real dotfiles directory. A dotfiles layout with
ordinary modes (`0755` directories, `0644` files) produces no warning.

```text
warning: <path> is group-writable (mode 0775, owner uid 501); Stache runs commands from its configuration, see docs/architecture/config.md#trust-model
```

`config path` prints these lines after its entries and notes. The desktop host
logs one `warn` record listing all of them at startup, none when clean; reloads
and watcher batches do not repeat it. No warning changes a file's acceptance,
the registry, the watcher or the tray. The control socket's same-user reach is
described in [ipc.md](ipc.md#endpoint-safety).

## Migration

`config migrate [--from PATH] [-f]` converts one of two sources to `<id>.toml`
files in the selected configuration directory (or `--config`). The report's first
line names the mode and the source (`migrating per-module files from <dir>`,
`migrating per-module file <path>` or `migrating legacy file <path>`). Sources are
never modified, and a source that is also a planned destination is refused even
with `--force`. Collisions with an existing `<id>.toml` refuse without `--force`,
which replaces only planned files. Everything is parsed, converted and validated
before the first file is written, so an invalid source writes nothing.

Source selection, in order (`select_source` in `cli/src/migrate.rs`):

1. `--from DIR` selects per-module mode over that directory. A directory without
   any registered `<id>.jsonc` fails and lists the expected names.
2. `--from FILE` named `<registered id>.jsonc` (including `global.jsonc`) selects
   per-module mode for that one file. Any other file is the legacy single file.
   A missing path fails.
3. Without `--from`, per-module mode runs when the configuration directory holds
   at least one registered `<id>.jsonc`. A legacy `config.jsonc`/`config.json` in
   the same directory is then reported as not migrated, with the `--from` command
   that would migrate it.
4. Otherwise legacy discovery runs (below).

### Per-module mode

This is the upgrade path for files written before the TOML switch. Only registered
ids with an existing `<id>.jsonc` are converted; no template is written for the
others (the loader's defaults and `config init` cover them). For each file the
JSONC parser reads it (comments and trailing commas only), a root `$schema` key is
dropped silently, the schema-driven converter renames camelCase and kebab-case keys
and enum values to snake_case, and the catalog validates the result before it is
written with `#:schema` on line 1. The `migrate_legacy_section` hooks do **not**
run, because these files already have the current shape (a keybindings file keeps
`{ enabled, bindings }` and is not wrapped again). Unknown keys are preserved with
the usual warning, and values above `i64::MAX` or nulls in unknown keys are handled
as for the legacy file. The last report line says that the `.jsonc` files are
unchanged and ignored and can be removed after checking the `.toml` files.

### Legacy single file

Legacy discovery uses `--from FILE` or the first existing legacy file, checking
`.jsonc` before `.json` at: `$XDG_CONFIG_HOME/stache/config`,
`~/.config/stache/config`, `~/Library/Application Support/stache/config` and
`~/.stache`. Duplicate paths are removed. The first existing source is
authoritative: if it is invalid, migration fails instead of trying a lower
priority file.

The legacy root `execOnStartup` and a `logging` section are converted into
`global.toml` as `exec_on_startup` and `logging`. Legacy root section names are
catalog ids and are never renamed.
For each registered section the order is: hook, schema-driven conversion,
typed validation, TOML write. The converter (`cli/src/migrate/convert.rs`) is a pure function
over the JSON value and the type's generated schema. It renames an object key to
snake_case only where the schema declares that snake form as a property, leaves
`additionalProperties` map keys alone, and normalizes a string only where the
schema location is a string enum (`enum` or `const` branches). Untagged unions use
the union of their branches. Undeclared keys are left as written and surface as the
usual unknown-key warnings; a camel/snake pair for one key fails migration naming
both. The legacy source is read as JSONC (comments and trailing commas only);
the output is TOML in `<id>.toml` files.

Every registered module section is decoded and validated by the catalog's
authoritative type: `bar`, `commandQuit`, `keepAwake`, `keybindings`, `menuAnywhere`, `proxyAudio`, `notunes`, `tiling` and
`wallpapers` today. A type may also translate legacy spellings first through
`ModuleConfig::migrate_legacy_section`, which migration calls before conversion and
writing; that hook is the only place a configuration type knows about values the
running configuration rejects. Hooks run first and match the legacy camelCase
spellings of the old file.
`notunes` uses it to turn the legacy `targetApp` preset names into a bundle
identifier and to drop an explicit block-only value. A legacy section that was
enabled and had no `targetApp` used to mean Spotify; the migrated file now blocks
without launching anything, and the hook returns a migration warning that names
that change. `keybindings` uses it to wrap the legacy shortcut map into
`{ enabled, bindings }`. `wallpapers` uses it to preserve a bare relative `path` or list
entry while warning that the new base is the destination configuration
directory, not the unrecoverable legacy working directory; absolute and
home-anchored values are silent. `bar` uses it to keep explicit `height`/`padding` (including `0`),
materialize an omitted one as 28/12 with a warning that the legacy application
rendered an omitted value as 0, and warn that a relative `weather.apiKeys` (written
`api_keys`) now
resolves under the destination configuration directory; unknown keys are kept
with the usual warning and a missing section receives the disabled template. `commandQuit` is decoded through its typed
configuration like the others: a valid section keeps its `enabled` and
`hold_duration`, an invalid one fails migration and an absent one receives the
disabled default template. `menuAnywhere` keeps the default hook: it is
validated generically and its `[]` modifiers and `middle_click` are written
unchanged. `tiling` is validated by `TilingConfig`, keeps every valid legacy
value and shape, and its hook only warns about planned runtime corrections: a
grid layout (the new cap is ten tiles, not twelve), an enabled section whose
floating `default_position` is `center` or omitted (it will start centering newly
floating windows once) and an enabled section with no `default_layout` (dwindle
is the single default). Nothing is rewritten and the module stays inert.
Invalid tiling input fails migration. No legacy module section is copied
verbatim any more. Legacy `widgets` are dropped with a
warning. Unknown root keys warn, and unknown keys inside a registered module
section are reported while the section is still validated and copied into its
`.toml` file, where the typed configuration ignores them. TOML has no null and
no integer above `9223372036854775807`: a null under a typed field is dropped
silently (it equals an absent optional), an unknown key that holds a null
anywhere inside it is dropped with ``warning: dropped `<path>` because TOML
cannot hold null``, and a larger integer in an unknown key fails migration with
``cannot write <file>: `<path>` is larger than the largest TOML integer`` before
anything is written. Paths use `a[0].b` form. A missing registered
module section is emitted as the same commented template as `config init`,
reported generically as `emitted default template <file>` with ` (disabled)`
when the registered default is disabled. `keepAwake` therefore always migrates disabled, whether the legacy `bar`
section was enabled, disabled or absent. `wallpapers` likewise always migrates
disabled; no legacy consent is inferred. Legacy has no Keep Awake setting, so
migration never creates or infers a separate enable flag and never treats Bar
consent as Keep Awake consent. Collision and `--force` rules match `init`. Every
validation, copy, drop and warning is printed.

This is a breaking layout change: the loader reads only `<id>.toml`, so after
updating run `stache config migrate` once (or `config init`).

## Schemas

Each catalog entry derives a per-file schema from `schemars`. `$id` is the
committed URL under `schemas/`, and templates and migrated files point their
first-line `#:schema` comment at it.

```sh
bun run generate-schema
bun run generate-schema -- --check
```

The `generate-schema` binary writes `schemas/global.schema.json`,
`schemas/bar.schema.json`, `schemas/commandQuit.schema.json`, `schemas/keepAwake.schema.json`, `schemas/keybindings.schema.json`,
`schemas/menuAnywhere.schema.json`, `schemas/notunes.schema.json`, `schemas/proxyAudio.schema.json`, `schemas/tiling.schema.json` and
`schemas/wallpapers.schema.json` deterministically; `--check` writes nothing and
exits non-zero on missing or stale files. Templates are generated from the
`Default` serialized straight to a TOML document plus schema descriptions, so
there is no second hand-maintained file. Keys follow struct declaration order
(`serde_json/preserve_order` is not enabled; the TOML document keeps the order),
scalars come before sub-tables, non-empty arrays of objects are
`[[arrays of tables]]`, an empty array is `key = []` and an empty map is `{}`,
and map keys that are not bare-key safe (`"CapsLock+a"`, `"§"`) are quoted. A
description becomes `#` comment lines above its key or `[table]` header (above the
first `[[element]]` of an array of tables). No `null` is ever written; an
`Option` field must use `skip_serializing_if`, which a catalog test enforces
over the composed app catalog.

Tests use temporary directories and injected `ConfigEnvironment` values; no
test reads the real home or changes an environment variable.
