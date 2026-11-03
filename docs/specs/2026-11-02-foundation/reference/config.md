# File-backed configuration

Read when reviewing the planned config system, generation or migration; part 3 draft.

Materialize as `docs/architecture/config.md`; schema/examples must come from implemented Rust types.

## Files and parsing

Default directory is nonempty $XDG_CONFIG_HOME/stache, otherwise ~/.config/stache. Global --config/-c
accepts a directory before/after subcommands, including --config=DIR. global.jsonc holds logging and
execOnStartup. Each registered module uses its exact ID: proxyAudio.jsonc contains enabled/input/output.

jsonc-parser accepts comments/trailing commas with other loose syntax explicitly disabled. Missing
initial files use typed defaults; malformed supplied files are diagnosed, never silently defaulted.
Schemas and commented templates derive from serde defaults/schemars descriptions. $schema is metadata.

## Reload and writes

Directory watcher handles atomic saves, debounces 200 ms and ignores unchanged content. Only affected
module reconfigures; enabling/disabling starts/stops it. Candidate revision commits after successful
application. Parse/type/validation/application rejection or deletion retains last-good state and diagnostic.
No full-app restart. Paused modules remain paused with updated accepted config. Log level changes in
place; execOnStartup runs only at first desktop setup, never during reload.

Tray enable changes use atomic edits preserving other JSONC content and refuse invalid/conflicting disk
files. Settings UI can later use the same file-backed operation, not an independent state database.

## Commands

- schema [global|proxyAudio]: global when omitted, one pretty schema per invocation.
- config init [-f] [-p DIR] [--stdout]: directory override, preflight collisions, force affects planned
  files only. Stdout prints deterministic filename-comment-delimited multi-file preview, no writes.
- config path: directory/files plus exists/active markers and diagnostics; label local validation offline.
- config migrate [--from FILE] [--force]: explicit source or ordered legacy discovery, destination is
  selected new directory. Source untouched, collision preflight before writes.
- reload: remote reread of all files, silent success, rejection/no host exits 1 with diagnostics.

Migration checks .jsonc then .json at XDG/stache/config, ~/.config/stache/config,
~/Library/Application Support/stache/config and ~/.stache, deduplicating paths. Invalid first source
errors. Fully convert globals/proxyAudio; preserve unported sections as same-ID JSON files without
activating them. Warn for widgets removal and unknown keys. New loader never reads legacy config files.

Generated schemas live under schemas/. `bun run generate-schema -- --check` must detect drift without
rewriting source. Tests use temporary homes/paths, not the user's live config directory.
