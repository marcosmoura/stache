# Configuration

Read when changing configuration files, reload behavior, schema generation,
migration or the `config`/`schema` commands.

Configuration is file-backed and typed. `stache-sdk` owns the parser, catalog,
store and watcher; the application composition registers the concrete types;
`stache-cli` implements the local commands without importing app or module
crates.

## Files and paths

The directory is, in order: an explicit `--config DIR`, a non-empty
`$XDG_CONFIG_HOME/stache`, then `~/.config/stache`. `-c DIR` and
`--config=DIR` are accepted before or after a subcommand. `init`, `path` and
`schema` never require the directory to exist.

Each file is its own catalog document, named after the exact identifier:

```text
~/.config/stache/
  global.jsonc       GlobalConfig
  bar.jsonc          BarConfig
  commandQuit.jsonc  CommandQuitConfig
  keepAwake.jsonc    KeepAwakeConfig
  keybindings.jsonc  KeybindingsConfig
  menuAnywhere.jsonc MenuAnywhereConfig
  notunes.jsonc      NoTunesConfig
  proxyAudio.jsonc   ProxyAudioConfig
  wallpapers.jsonc   WallpapersConfig
```

The loader reads only `global.jsonc` and registered IDs. Legacy
`config.jsonc`/`config.json` locations are read only by `config migrate`.

`bar.jsonc` is registered for configuration, schema, template and migration
only (`enabled` default `false`, `height` 28, `padding` 12, and `weather` with
`provider` `auto`|`visual-crossing`|`open-meteo`, `apiKeys` and `defaultLocation`).
Bar has no runtime, window, provider or command: no factory is registered, so
`enabled: true` validates but nothing runs. Dimensions keep the legacy `u16`
range and an explicit `0` is preserved, never replaced or clamped; feasibility
is a later runtime concern. `WeatherConfig::api_keys_path` trims `apiKeys`,
expands injected `~`/`~/`, resolves anything else under the configuration
directory and never expands shell or environment syntax or uses the process
working directory. Nothing reads the env file. See [bar.md](../features/bar.md).

`notunes.jsonc` is registered for configuration, schema, template and migration,
and backs the shipped blocker runtime: enabling it installs application
observation and terminates Apple Music/iTunes. Catalog support alone starts no
module, creates no webview and installs no native observation; see
[notunes.md](../features/notunes.md).

`keybindings.jsonc` is registered for configuration, schema, template and
migration and drives the Keybindings module (see [keybindings.md](../features/keybindings.md)).
An accepted reload is observed by the module through its accepted-config watch: a rejected or
unchanged file changes nothing and cancels no running command, an accepted changed file
invalidates the old dispatcher generation. `KeybindingsConfig`
has `enabled` (default `false`) and `bindings` (default `{}`), a map from a
shortcut string to one command string or an ordered list of strings (the same
wire shape as `execOnStartup`). Validation is shape-only: non-string values and
array elements are rejected with field paths, while a malformed shortcut spelling
is not a file rejection (planning diagnoses it per entry in the module's runtime status). Migration
wraps the whole legacy map as `bindings` and sets `enabled` from map
nonemptiness, even when every command is blank; a legacy shortcut literally named
`enabled` or `bindings` is a binding, never the new envelope. A missing legacy
section emits the disabled template. See [keybindings.md](../features/keybindings.md).

`menuAnywhere.jsonc` backs the MenuAnywhere runtime: the host registers it
through `MenuAnywhereFactory`, so the file is bound, watched and reloaded like
any module file. Registration is inert (including an enabled file); only an
accepted `enabled: true` with Accessibility granted starts the module, and the
runtime is documented as implemented but with native behavior unverified.
`MenuAnywhereConfig` has `enabled` (default `false`), `modifiers` (default
`["control", "command"]`; `control|option|command|shift`) and `mouseButton`
(default `"rightClick"`; `rightClick|middleClick`); `{}` uses the per-field
defaults, unknown keys warn, and unknown enum spellings, wrong types, `null`
fields and non-object roots are rejected with field paths. Empty `modifiers`
(`[]`) is valid and captures unmodified matching clicks system-wide while
enabled; the schema description and template comment
say so. Duplicates are valid, preserved in the file and count once. The crate
also provides a pure four-bit `ModifierMask` whose equality matching ignores
unrelated flags such as Caps Lock. `middleClick` means button number 2 only, so
back and forward buttons never match. See
[menu-anywhere.md](../features/menu-anywhere.md).

`keepAwake.jsonc` is registered for configuration, schema, template and
migration, with only `enabled` (default `false`). It backs the shipped Keep
Awake runtime: an accepted `enabled: true` starts one module generation that
manages the lock-aware power assertion, while `false` stops it. Construction,
registration and configuration acquire nothing. A rejected parse, type, I/O or
deleted file keeps the last accepted revision and live lease and records a
diagnostic; identical last-good bytes clear it without lease churn. See
[keep-awake.md](../features/keep-awake.md).

`wallpapers.jsonc` is registered for configuration, schema, template and
migration but has no runtime in this release: nothing binds it, so the watcher
and `reload_all` skip it and `core.status` shows it valid but not active.
`WallpapersConfig::validate` is structural only. Source resolution is a separate
shared API that resolves the trimmed `path` or `list` against the configuration
directory and injected home, and an explicit CLI file against the invocation
CWD. See [wallpapers.md](../features/wallpapers.md).

`commandQuit.jsonc` is bound to the [Hold-to-Quit](../features/hold-to-quit.md)
module (`CommandQuitConfig`, default-disabled). Its keys are `enabled`
(default `false`), `holdDuration` (milliseconds, default `1500`) and
`dismissDelay` (milliseconds the HUD stays after an early release, default
`1500`, `0` hides at once), `position` (`"bottom"` default, `"top"`, `"left"`,
`"right"`, `"center"`: where the HUD appears) and `animation` (`"fade"` default
or `"slide"`: how it enters and leaves; Reduce Motion makes a slide a fade).
`position` and `animation` are lowercase string enums exported to the generated
TypeScript; an unknown string or wrong type is a type error naming the key.
Every `u64`, including `0` and `u64::MAX`, is valid
for both durations and the JSON Schema keeps the full integer range; wrong
types and overflow are type errors naming the key. Because TypeScript `number`
cannot represent every `u64`, both are excluded from the generated TypeScript
(`#[specta(skip)]`); a consumer that needs one must approve an explicit
representation first. A migrated legacy file has none of `dismissDelay`,
`position` or `animation` and decodes to the defaults (`1500`, `bottom`, `fade`). A reload to a different
value cancels any in-flight hold and arms new presses only after the matching
accepted publication. See [hold-to-quit.md](../features/hold-to-quit.md).

## Parsing

`jsonc-parser` runs with comments and trailing commas enabled and every other
loose option disabled: no single quotes, missing commas, hexadecimal, unary
plus, bare decimals, non-finite numbers or extended escapes. A `$schema` key is
ignored metadata. Any other key the target type does not declare is ignored and
reported as a non-fatal warning, so a typo never invalidates the file. A
whitespace-only file is invalid with the message ``file is empty; write `{}` to
use defaults``.

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
never executes `execOnStartup`.

## Tray enable edits

The tray's configuration-backed **Enable** action does not reserialize the file.
`plan_enabled` reads the current bytes (or the generated template when the file
is missing), refuses a symlink or an unreadable/invalid current file, locates the
root `enabled` boolean with the JSONC AST and validates the complete replacement
text before any write. The commit prepares and syncs a same-directory temporary
file first, then re-reads the original bytes as close to the rename as possible.
A missing target uses a no-clobber rename, so a concurrently created file is
never replaced. Comments, trailing commas, key order and unrelated values are
preserved. A non-cooperating writer can still change the file between that last
check and the rename; compare-plus-rename is not an atomic compare-and-swap on
POSIX. Refusing a configuration file that is itself a symlink is deliberate: the
tray never replaces the link. A replacement keeps the original file's permission
bits, but the rename creates a new inode, so hard links to the old file are not
updated.

After a successful commit the host calls `ConfigStore::reload_one` once. The
watcher later reports the same bytes as unchanged, so a module is not reconfigured
twice. An unchanged enabled value still syncs the disk state and retries an
eligible failed start instead of only clearing the error. **Pause/Resume** never
edit a file; a disabled module cannot be resumed from the tray. File diagnostics
stay visible separately from runtime status.

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

`global.jsonc` updates the injected `LogFilter` immediately, so level changes
are live. `execOnStartup` is read once by the desktop host, never by config
loading, reload or CLI. The host starts it once after initial setup (after
registry start, socket and watcher) on its own owned thread, sequentially with
legacy quoting rules, stopping at the first failure; shutdown cancels it, kills
only the child it spawned directly and does not start later commands. A child not
confirmed gone within 2 s is retained by the runner with a `stache: startup:`
stderr diagnostic instead of being waited for indefinitely. The exact bare name
`stache` runs the running Stache Next executable (re-entered with the hidden
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
stache config migrate [--from FILE] [-f]
stache schema [global|bar|commandQuit|keepAwake|keybindings|menuAnywhere|notunes|proxyAudio|wallpapers]
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
nothing and prints `// file: <name>` blocks. `-p` wins over `--config`, which
wins over the default.

## Path command markers

`path` prints the directory and expected files. Existing valid files are
`(exists, offline)` in a standalone run; a reachable host may mark them
`(exists, active)`. An invalid file is `(exists, invalid)` and prints its
diagnostic. Unknown-key warnings print under their file without changing its
marker. Missing files have no marker. Offline means the local file validates,
not that a host accepted it.

`config path` and the `schema` output never write ANSI when piped. `schema`
styles keys, strings, numbers and literals only when stdout is a terminal; see
[ipc.md](ipc.md#output).

`schema` defaults to the global schema; `schema bar`, `schema commandQuit`, `schema keepAwake`, `schema menuAnywhere`,
`schema proxyAudio`,
`schema notunes` and `schema wallpapers` print those registered module schemas.
Unknown IDs fail.

## Migration

`config migrate` uses `--from FILE` or the first existing legacy file, checking
`.jsonc` before `.json` at: `$XDG_CONFIG_HOME/stache/config`,
`~/.config/stache/config`, `~/Library/Application Support/stache/config` and
`~/.stache`. Duplicate paths are removed. The first existing source is
authoritative: if it is invalid, migration fails instead of trying a lower
priority file. The source is never modified: if it is also a planned
destination, migration refuses even with `--force`.

`execOnStartup` and a `logging` section are converted into `global.jsonc`.
Every registered module section is decoded and validated by the catalog's
authoritative type: `bar`, `commandQuit`, `keepAwake`, `keybindings`, `menuAnywhere`, `proxyAudio`, `notunes` and
`wallpapers` today. A type may also translate legacy spellings first through
`ModuleConfig::migrate_legacy_section`, which migration calls before decoding and
writing; that hook is the only place a configuration type knows about values the
running configuration rejects.
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
rendered an omitted value as 0, and warn that a relative `weather.apiKeys` now
resolves under the destination configuration directory; unknown keys are kept
with the usual warning and a missing section receives the disabled template. `commandQuit` is decoded through its typed
configuration like the others: a valid section keeps its `enabled` and
`holdDuration`, an invalid one fails migration and an absent one receives the
disabled default template. `menuAnywhere` keeps the default hook: it is
validated generically and its `[]` modifiers and `middleClick` are written
unchanged. `tiling` is copied verbatim as a same-ID `.jsonc` file; the
loader does not register or activate it. Legacy `widgets` are dropped with a
warning. Unknown root keys warn, and unknown keys inside a registered module
section are reported while the section is still validated and copied into its
`.jsonc` file, where the typed configuration ignores them. A missing registered
module section is emitted as the same commented template as `config init`,
reported generically as `emitted default template <file>` with ` (disabled)`
when the registered default is disabled; absent unported sections create no
file. `keepAwake` therefore always migrates disabled, whether the legacy `bar`
section was enabled, disabled or absent. `wallpapers` likewise always migrates
disabled; no legacy consent is inferred. Legacy has no Keep Awake setting, so
migration never creates or infers a separate enable flag and never treats Bar
consent as Keep Awake consent. Collision and `--force` rules match `init`. Every
validation, copy, drop and warning is printed.

This is a breaking layout change: the new loader never reads the legacy single
file, so run `config migrate` once (or `config init`) after switching.

## Schemas

Each catalog entry derives a per-file schema from `schemars`. `$id` is the
committed URL under `schemas/`, and templates point `$schema` at it.

```sh
bun run generate-schema
bun run generate-schema -- --check
```

The `generate-schema` binary writes `schemas/global.schema.json`,
`schemas/bar.schema.json`, `schemas/commandQuit.schema.json`, `schemas/keepAwake.schema.json`, `schemas/keybindings.schema.json`,
`schemas/menuAnywhere.schema.json`, `schemas/notunes.schema.json`, `schemas/proxyAudio.schema.json` and
`schemas/wallpapers.schema.json` deterministically; `--check` writes nothing and
exits non-zero on missing or stale files. Templates are generated from the
serialized `Default` plus schema descriptions, so there is no second
hand-maintained file.

Tests use temporary directories and injected `ConfigEnvironment` values; no
test reads the real home or changes an environment variable.
