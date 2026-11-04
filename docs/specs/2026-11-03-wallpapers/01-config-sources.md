---
status: draft
---

# 01 — Config and sources

Read when implementing the planned config/source foundation. Depends on no later part.

## Scope

Create the compilable `stache-wallpapers` config/source library without a
production runtime or wallpaper action. Touch root workspace membership, the
new crate, `src/crates/app/src/{catalog,bindings}.rs`, schema generation,
`src/crates/sdk/src/{registry.rs,config/}` and
`src/crates/cli/src/migrate.rs` as required. Conditional generic factory-context
plumbing also touches `src/crates/app/src/host.rs`; reuse prior landed context.
Manifest/generated edits are for
implementation, not this docs-only proposal.

## Requirements

### Typed config and pool

- Register ID `wallpapers`, file `wallpapers.jsonc`; derive serde, schemars and
  Specta using the existing Rust collection. Defaults remain `enabled: false`,
  `path: ""`, `list: []`, `interval: 0`, `mode: "random"`, `radius: 0`,
  `blur: 0`. Preserve field types (`interval` u64, effects u32), random/sequential
  values, field defaults, unknown-key warnings and JSONC behavior. No new public
  limits/knobs or schema-time filesystem requirements.
- Config/schema/template commands perform no source image decoding or native
  screen enumeration. Structural validation is distinct from runtime source I/O.
  Default empty sources remain valid configuration, not a schema rejection.
- Preserve legacy config-string trimming for `path` and each `list` entry before
  tilde expansion; this does not trim literal explicit CLI filesystem arguments.
  Resolve `~`/`~/` with injected home context; absolute paths stay absolute;
  other paths use the config file's directory, never process CWD. Do not add
  arbitrary shell/environment expansion. A nonempty directory `path` overrides
  `list`, including when that directory is invalid; do not silently fall back.
- Directory enumeration is nonrecursive, regular image files only, with
  case-insensitive `jpg`, `jpeg`, `png`, `webp` extension checks and natural
  ordering (deterministic path tie-break). Explicit list order/duplicates remain
  meaningful. Preserve filtering of missing/unsupported list entries instead
  of rejecting every historically valid mixed list. Do not decode on `list`.
- Distinguish an empty usable pool (cannot serve rotation/random) from bad
  directory/read errors and structurally invalid config. Validate source I/O
  transactionally at start/reconfigure; rejection leaves last-good runtime.
  This is not a new source-directory watcher. Later processing handles a file
  deleted/corrupted after pool construction without changing the source.
  Empty enabled rotation maps to an actionable typed failed start and generic
  Retry under the shipped SDK, not an invented missing-source unavailable reason;
  random fails for an empty pool while local list can return empty successfully.
- Provide shared source resolution usable by local list/process/set and runtime.
  With rotation disabled, an actual explicit file can work despite an empty or
  unusable configured pool; malformed structural config still fails. A running
  host uses its last accepted config; standalone never invents last-good values.

### Accepted snapshot SDK checkpoint

- Add a narrow generic typed watch/read handle returning accepted config plus
  its exact revision atomically from published registry/store state, with bounded
  cancellable revision-change wait/notification from that same cell. Design the concrete
  signature during implementation; retain typed handles and wrong-type compile
  protection. This is not `ConfigStore::initial`, `core.status`, rereading disk,
  calling module code or a tool-specific SDK/host switch.
- Return absent for malformed/rejected initial config (revision zero); return
  accepted defaults for a never-existing default-valid file. Include paused/
  disabled staged updates and config committed despite a later start error.
  Rejected/deleted reloads retain the last accepted pair. A later valid fix of
  malformed initial config becomes visible without host restart.
- Publish metadata under a small independent lock; reads must not acquire a
  module/lifecycle/reload lock or wait for start/stop/native work. A concurrent
  reader sees an entire last-published pair, never mixed revision/config.
- Publish the pair before notifying revision waiters; the check/wait protocol
  cannot miss publication between a read and sleeping. Stop/cancellation also
  wakes waits. Coalesced/stale/spurious wakes reread the authoritative pair;
  a lossy event alone, polling or spinning until the old timer deadline is not
  sufficient. This narrow publication notification is the explicit exception to
  the prohibition on broader SDK frameworks/lifecycle hooks.
- Prove registration/binding and handler access with fake SDK clients now.
  `ModuleFactory::ipc_handlers()` currently takes no context. If needed, add
  thin generic factory/handler context plumbing to pass a typed snapshot source
  or store/registry reference, preserving proxyAudio and factory tests. Do not
  create a general service framework or ad-hoc module/global config mirror.
- `Registry::reload_inner` calls `Module::reconfigure` before replacing config/
  revision and publishing metadata. The accessor must retain the old accepted
  pair throughout that window and expose the new pair only on acceptance. A
  preflighted module pool is not accepted-state evidence. Part 03 stages its
  matching candidate until publication, matching typed config equality AND the
  actual published revision, never guessed revision + 1. Under the current
  registry, successful running reconfigure leads infallibly to config/revision
  assignment and RAII publication; prove that invariant with tests rather than
  inventing a generic commit callback. Concrete watch signatures are compile-
  proved in this part, not claimed as already shipped.

### Migration

- Replace Wallpapers' `UNPORTED` verbatim-copy case with catalog-driven typed
  migration registration; keep policy in the owning config/module contribution,
  not a Wallpapers branch in host/CLI. Generalize existing typed migration only
  as needed. Validate converted values through the authoritative type.
- Keep source immutable, source-is-destination refusal (even force), collision
  preflight, force behavior, file modes, unrelated sections and warnings. Existing
  absent-section/template policy must be explicit in tests; registered Wallpapers
  may emit its disabled default template, never enable itself.
- Absolute/home-anchored sources preserve meaning. For old bare relative paths,
  historical CWD cannot be recovered from the legacy file location: preserve the
  text with an actionable warning that the new base is the destination config
  directory and manual rebasing may be required. Never assert inferred fidelity.

## Acceptance criteria

- [ ] WP01-01: Workspace/new crate compile; generated config/schema/Specta entries agree
      with defaults and commands without starting a module or decoding images.
- [ ] WP01-02: Shared pool resolver preserves precedence, filtering, ordering and explicit
      path independence; errors distinguish structural/source/argument failures.
- [ ] WP01-03: Generic accepted watch/read handle passes disabled/paused/rejected/start-
      failure tests and compile-proof typed usage without filesystem reads.
- [ ] WP01-04: Migration is catalog-driven, warns on ambiguous relative meaning, preserves
      immutable source/collisions/modes and all unrelated tool behavior.
- [ ] WP01-05: Feature/config/SDK/new-module docs describe only this shipped slice; full
      runtime/CLI remains marked planned. Overview validation gates pass.

## Tests

Temporary config/home/CWD fixtures: defaults, omitted fields, malformed/empty
file, unknown keys, numeric boundaries, schema determinism, path versus list,
trim-before-tilde config strings versus literal CLI filename spaces,
tilde/absolute/config-relative paths, natural `2`/`10` order, nested exclusion,
mixed-case extensions, duplicate/list order, filtered/all-invalid list, missing
directory and permission/read failure. No real home/environment mutation.

SDK fake modules: never-existing defaults, malformed initial then valid fix,
disabled and paused multi-reload, rejected/deleted reload, committed config with
failed start, atomic pair reads, barriers holding start/reconfigure/stop while
snapshot reads complete, and an injected/read-count proof of no disk access.
Use a deterministic barrier after reconfigure preflight but before accepted
publication: typed readers retain the old pair; acceptance publishes the complete
new pair, rejection preserves the old pair, and wrong-config-type usage fails
to compile. No module callback or disk read is needed by the accessor.
Test publication between check/wait, rapid/coalesced A–B–A updates, same config
at different revisions, no missed wake, initial disabled/paused state and bounded
cancel/stop wake with an old long interval. Assert pair atomicity and the current
successful-preflight-to-publication invariant.
Migration fixtures cover ambiguity warnings, force/no-force, source identity,
mode preservation, untouched sections and missing registered sections.

## Notes

This part is useful and testable without native processing or factory startup.
No accepted-state accessor is currently shipped; compile-proof it here rather
than letting Part 03 substitute current disk or a stale module cache.
