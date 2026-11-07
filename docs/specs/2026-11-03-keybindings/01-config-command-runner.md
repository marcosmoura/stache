---
status: done
---

# 01 — Configuration and shared command runner

Read when implementing the catalog-only module and extracting the existing
startup runner. Depends on rebased current main, not unmerged sibling modules.

## Scope and files

- Add `src/crates/modules/keybindings/{Cargo.toml,src/lib.rs,src/config.rs}`;
  register its config in `app/src/catalog.rs`, workspace/app manifests and
  `app/src/bindings.rs`. No production factory yet.
- Add `sdk/src/commands.rs` (planned) and exports; move shared parser, executable
  resolution and direct-process mechanics from `app/src/startup.rs`. Keep
  `StartupJob`'s one-shot host ownership in app. Adapt existing host tests/imports.
- Extend `cli/src/migrate.rs` tests and app `config_catalog`, `generator`,
  `cli_binary`, `bindings` tests; generated schema/bindings change only later
  during implementation. Remove `keybindings` from `UNPORTED` when registered.
- Extend `app/src/lib.rs::dispatch`, `cli/src/{commands.rs,mode.rs}` and app
  executable/mode-selection tests only for the internal child CLI path below.

## Requirements

`KeybindingsConfig` derives the existing catalog/schema/binding traits with only
`enabled` and `bindings`, defaults false/empty. Values retain string/array shape
and order; use a shared or module-owned transparent command value without
renaming the shipped `StartupCommands` wire shape. No shortcut/native/process
side effects in config validation, schema, init, migration or CLI construction.
Malformed shortcut syntax is a later per-entry planning diagnostic, not a typed
file rejection. Non-string values/array elements reject with field paths.

Use `ModuleConfig::migrate_legacy_section`: wrap the entire legacy map as
`bindings`, set enabled from map nonemptiness (even blank commands). Do not
mistake legacy shortcuts named `enabled` or `bindings` for the new envelope.
Nonobject sections remain invalid; missing legacy section emits the catalog's
disabled template. Reuse source protection, collision/force preflight and atomic
per-file writes; never import `_legacy` or change noTunes/proxyAudio conversion.

The SDK runner exposes typed step failures so the startup adapter reproduces
existing `startup command N: …`, `… is empty`, `… could not run: …`,
`startup command N/total exited with …`, and `… could not be waited for: …`
messages. Cancellation remains nonfailure for startup. Preserve single/double
quotes, escaped quotes/backslashes, empty quoted arguments, trailing escape and
unterminated-quote errors; no shell expansion, pipes, redirects or implicit shell.
Normalize effective blank commands at the caller as today; the low-level parser
must still distinguish an empty invocation from an empty quoted argument.

Resolve absolute executable paths directly; exact bare `stache` uses injected
running Next executable first and fails truthfully if unavailable, with no
legacy/debug-directory/PATH fallback for that name. Other names search injected
`STACHE_EXTRA_PATHS`, `PATH`, `/usr/local/bin`, `/opt/homebrew/bin`,
`/opt/homebrew/sbin`, `~/.cargo/bin`, `~/.local/bin` in that order. Source-check
and freeze current startup treatment of empty path components rather than
silently changing it during extraction. Inherit cwd/environment/stdio unchanged;
neither config-relative cwd nor shell tilde expansion is added.

For exact parsed bare executable `stache` only, spawn the resolved current Next
executable with arguments `[--stache-internal-cli, ...original parsed arguments]`;
the OS supplies argv[0] normally. Preserve every original argument and its order,
including empty arguments; do not rewrite absolute/relative paths or other names.
It is a hidden, root-only, nonrepeatable mode flag, not a public command or
environment switch. Let the composed parser reject a second recognized mode flag;
do not scan/reject literal arguments after `--` or rewrite argument values merely
because their text equals the marker.
The same composed CLI parser validates it in both standalone and bundle paths:
it conflicts with `--desktop`, requires a parsed subcommand (missing command is
usage exit 2), and never turns unknown commands/flags into successful desktop work.
Help/version retain parser exit/output precedence, with no host construction.
After successful validation this flag selects CLI before bundle precedence;
without it retain all existing direct/Finder/`--desktop` precedence unchanged.
CLI dispatch uses the existing built-in/contributed catalog and propagates actual
exit status/output, never an instance-lock desktop success. No global environment
mutation or legacy PATH binary workaround. `stache reload` must work through this
path; later registered `stache tiling ...` uses it unchanged, while currently
unregistered `tiling` fails as an unknown command. Do not add Tiling here.

One sequence waits for each direct child and aborts at first parse, resolution,
spawn, wait or nonzero-exit failure. No command-duration deadline in this legacy
mode. The shared spawn gate rechecks accepted generation/revision/cancellation at
process creation, not merely when dequeued; startup supplies its one-shot
cancellation scope. Cancellation kills/reaps only the directly owned child and
prevents later steps. Do not kill descendants, sessions, applications or a process
found by name. Allow **2 s** for direct-child cancellation cleanup, then transfer
the exact handle to a bounded owned reaper with diagnostics; never discard it or
strand an unjoinable registry worker. Reserve from **64 retained-child slots**,
owned by the runner instance that spawned the child (the app startup runner or one
module runtime), not a host-wide reaper; exhaustion blocks new children, not
ownership transfer. Release reservations on confirmed reap. Cancellation-aware
wait/poll quantum is at most **10 ms**. These engineering constants are not user
config. OS scheduling/uninterruptible operations can overrun elapsed targets;
returning pending never proves termination. No new command threads in SDK per
invocation; the caller owns dispatch capacity.

Provide a second **captured** mode for callers that must bound output and time:
a caller-supplied deadline, separate bounded stdout/stderr caps (caller chooses
byte limits within a documented maximum), streamed bounded draining while the
child runs, and cancellation. Overflow fails the call as a typed outcome rather
than returning truncated output as success. A capture timeout means the child was
terminated/reaped or is retained with an explicit pending/uncertain outcome; it
never implies the command did not act. Captured children follow the same ownership
and retained-handle rules. The inherited-stdio mode above stays byte-for-byte
unchanged for keybindings commands and `execOnStartup`. Part 04's hidutil calls and
later Tiling/Bar bounded helpers reuse this mode instead of writing their own
capture; no shell interpolation in either mode.

## Acceptance criteria

- [x] **KB-01-01** — Catalog, defaults, schema/template and exported config agree;
      construction performs no native registration, command or runtime work.
- [x] **KB-01-02** — Migration wraps maps correctly, preserves value order (keys are written in lexical order),
      validates types before writes and preserves all existing module conversions.
- [x] **KB-01-03** — Extracted parser/resolver passes compatibility vectors and
      resolves bare `stache` only to the injected running Next executable.
- [x] **KB-01-04** — Startup retains exact failure formatting, once-only timing,
      sequential failure/cancellation semantics, and inherited process context.
- [x] **KB-01-05** — Spawn/cancel races cannot issue a stale later step; direct
      children are reaped or retained with truthful bounded-cleanup diagnostics,
      without blocking main or unrelated teardown indefinitely.
- [x] **KB-01-06** — Internal CLI rewriting and validated mode selection compile
      and execute CLI-only in standalone/packaged invocation contracts, preserving
      ordinary bundle precedence and rejecting invalid requests without desktop work.
- [x] **KB-01-07** — Captured mode enforces deadline, separate output caps and
      cancellation with typed overflow/timeout outcomes, while the legacy
      inherited-stdio sequential mode and `execOnStartup` behavior stay unchanged;
      retained children are owned per runner instance.

## Tests and gates

Use injected environment, filesystem, process factory/child handles, barriers and
fake time. Cover absent/empty/nonempty migration, blank-only maps, reserved-looking
shortcut names, invalid shapes, collisions/source-is-destination/force, stable
generation and noTunes regression. Table-test archived and startup quoting;
literal shell metacharacters, quoted empty binary/argument, PATH precedence,
execute bits, missing HOME, absolute paths and competing legacy `stache`.
Use captured child argv plus app executable dispatch with a mocked desktop selector
that fails if called for the internal path, for both standalone and
`.app/Contents/MacOS/stache` executable identities. Cover `reload` with fake IPC,
a fake contributed command with future `tiling` argument shape, currently unknown
`tiling`, explicit config arguments, missing command, duplicate/misplaced flag,
`--desktop` conflict, invalid flags, help/version and unchanged direct bundle
precedence. Compile the actual packaged entry/selector and feed its rewritten argv
through the same dispatch seam; path-only resolver tests are insufficient. Never
launch a desktop, tray, webview, second host or real system command. If this contract
cannot be proven, stop for approval and block part 05 rather than waive it.
Prove cancellation before resolution, between resolution/spawn, during wait and
between steps; spawn/wait/kill/reap failures and retained-handle exhaustion must
not spawn another child. Fake time proves 10 ms polling, 2 s cleanup transfer and
64-slot reaper exhaustion. Prove captured mode's deadline, per-stream cap overflow,
streamed draining and cancellation outcomes, and that inherited-stdio mode emits
identical bytes/errors. Preserve existing startup host readiness/shutdown tests.
New tests never run configured real commands; harmless existing process fixtures
remain owned and isolated, not a native-input/manual gate.

Compile/test SDK/app/CLI plus the new crate with warnings denied; run
`bun run check:generated`, `bun run lint`, `bun run format:check`, `bun run test`,
`bun run build`, and `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps`.
No live native gate in this part. Update config/SDK docs for shipped catalog and
runner, and the feature notice for catalog-only status, without claiming hotkeys.
