---
status: done
---

# 03 — Config, generation and migration

Read when implementing typed files and local config commands.

## Scope and files

Create src/crates/sdk/src/config/{mod,paths,loader,watcher,schema,template,diagnostics}.rs,
src/crates/cli/src/{config,schema,migrate}.rs, schemas/{global,proxyAudio}.schema.json,
a generate-schema bin in stache-app, src/crates/app/src/catalog.rs and fixtures. Introduce config-only
src/crates/modules/proxy-audio/{Cargo.toml,src/lib.rs,src/config.rs}, depending on SDK; runtime is part 6.

## Key APIs

ConfigCatalog::register<C: ModuleConfig>(id) provides typed decode/validate/default/template/schema.
ConfigStore::reload_one(id) / reload_all() return per-file applied/unchanged/rejected outcomes and
independent diagnostics. Inject ConfigPaths, never mutate environment/shared globals in tests.

## Requirements

- Global `--config DIR`, `-c DIR`, `--config=DIR` work before/after subcommands. Init/path/schema do not
  require directory existence. Default: nonempty XDG_CONFIG_HOME/stache then ~/.config/stache. Load only
  global.jsonc and registered exact-ID .jsonc files, not legacy locations or unported sections.
- GlobalConfig: logging level/debug behavior and execOnStartup (string-or-list, empty default).
  ProxyAudioConfig: enabled/input/output, matching strategies and dependsOn camelCase as in
  `_legacy/app/native/src/config/types/audio.rs`; disabled/empty defaults.
- Choose jsonc-parser with serde feature, explicit comments/trailing commas true and all other loose/
  JSON5/missing-comma options false. Pin compatible version. Report file/field/parse location when
  available. Missing first-load files use typed defaults; malformed supplied files never silently do.
  Invalid initial module config is inactive with diagnostic; invalid global config reports through
  bootstrap logging and executes no startup commands. Empty objects allow per-field defaults.
- Watch parent directories (including initially absent directory creation), debounce 200 ms, handle
  atomic-save renames and unchanged content. Only affected module reconfigures; commit revision after
  successful application. Deleted/bad files after good load retain last-good state with diagnostic.
  Global log level changes live; execOnStartup never re-executes on reload.
- Generate commented defaults from Rust Default/serde plus schemars descriptions, not a second template.
  Accept $schema metadata in files. Use per-file schema IDs, not obsolete root schema. `schema` and
  `schema global` print global schema; `schema proxyAudio` prints pilot schema; unknown ID errors.
- `config init [-f/--force] [-p/--path DIR] [--stdout]`: destination path > global flag > default.
  Generate global plus registered modules. Preflight collisions; without force change nothing on
  collision. Force only replaces planned files. Stdout does no writes, prints deterministic
  `// file: <name>` delimited templates, global first then ID order (preview, not a single JSON file).
  Use atomic per-file writes; report IO failure without claiming cross-file transactionality.
- `config path` lists directory/expected files and `(exists)`/`(active)` markers. Active means accepted
  by reachable running host; offline means valid local file (label offline). Last-good with bad disk
  content shows diagnostic, never claims disk is healthy. Part 4 wires host diagnostics on stderr.
- `config migrate [--from FILE] [--force]`: explicit source or first existing in ordered legacy pairs
  (.jsonc before .json): XDG_CONFIG_HOME/stache/config; ~/.config/stache/config;
  ~/Library/Application Support/stache/config; ~/.stache. Deduplicate paths. Invalid chosen source
  errors rather than trying lower priority. Destination uses selected new directory; source untouched.
- Convert execOnStartup and supported global logging keys if present; validate pilot with authoritative
  type. Write unported bar/commandQuit/wallpapers/keybindings/menuAnywhere/notunes/tiling contents
  verbatim as JSON values into same-ID files (no wrapping; comments need not retain byte layout).
  Loader does not activate these files. Drop widgets with warning; warn for unknown keys. Emit global/
  pilot defaults if absent, no files for absent unported sections. Init collision/force rules apply;
  print every conversion/passthrough/drop and warning. No legacy-crate dependency.

## Acceptance criteria

- [x] C01: Directory resolution/flags, defaults and invalid initial-file behavior match requirements;
      legacy discovery happens only in migration.
- [x] C02: Comments/trailing commas work, other extensions/bad types fail visibly; types generate
      defaults/schema/templates without hand-maintained duplicates.
- [x] C03: Debounced hot reload/enable/disable/atomic saves affect only the file's module; bad/deleted
      config preserves last-good resources/revision and diagnostic, with no restart.
- [x] C04: Global level updates live; config code never executes startup commands.
- [x] C05: Init destination/force/stdout and non-destructive collision behavior have golden/IO tests.
- [x] C06: Path markers and offline/diagnostic hooks have defined, tested output.
- [x] C07: Migration discovery/conversion/passthrough/warnings preserve source/unrelated files.
- [x] C08: Schema selection/generation is deterministic and committed; check mode fails on drift
      without changing source.
- [x] C09: Pilot schema/migration/future runtime share one config-only type crate with no native start.

## Tests and validation

Temp-directory watcher tests with bounded waits, rejected reconfigure, delete/recreate/editor rename,
paused reload, bad initial/global config, migration paths/collision/partial IO failure, CLI goldens.
Run `bun run lint`, `bun run test`, `bun run generate-schema`,
`bun run generate-schema -- --check`, `bun run format:check`; exercise local CLI with temp config.
Introduce these scripts in root package.json; no real-home or desktop/device-changing test side effects.

## Docs

Write docs/architecture/config.md; update new-module/testing/index. Explain breaking migration,
offline markers, diagnostics and deletion semantics. Samples are generated, not hand-mirrored.
