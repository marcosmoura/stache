---
status: draft
---

# 04 — CLI and integration

Read when implementing the planned command contribution. Depends on Parts 01–03.

## Scope

Contribute the existing CLI from `src/crates/modules/wallpapers/src/cli.rs`
through `src/crates/app/src/commands.rs`; `stache-cli` stays SDK-only. Complete
command/host integration tests, generated-type checks and shipped feature docs
in this same part. No new CLI families or mutating Tauri/frontend commands.

## Requirements

### Compatibility and selection

Retain this command tree, flags, aliases and default target:

```text
stache wallpaper set [PATH] [--random|-r] [--screen|-s all|main|N]
stache wallpaper list
stache wallpaper generate-all
```

- Both PATH and random fail with exit 1 and stderr exactly
  `stache: Cannot specify both <path> and --random. Use one or the other.`
  Neither fails with exit 1 and
  `stache: Either <path> or --random must be specified.`
  Each includes the normal trailing newline. Validate these independently of
  source-pool setup; clap syntax failures retain foundation exit 2.
- Successful set prints `Wallpaper set successfully.` and exits 0 only for
  complete known success in the typed operation outcome, not merely transport
  `Response::Ok`. `list` prints a pretty JSON string array or
  `No wallpapers found.` and exits 0 for an empty valid collection. Invalid
  config/directory I/O is not an empty-success disguise. Use terminal-only ANSI
  through injected foundation styling; piped output contains no escapes.
- `all`/`main` are case-insensitive; main/1/legacy 0 target primary
  `NSScreen.screens[0]`. Numeric N is otherwise 1-based. Preserve parser error
  text from legacy `cli/commands/types.rs`; range-check against the one native
  snapshot before any decode/cache write, not via a 2K fallback.
- An actual supported regular file path wins over collection name lookup,
  including a bare existing filename in invocation CWD. Expand supported home
  syntax, resolve against invocation CWD and send absolute. This intentionally
  fixes the legacy restriction to configured basename matches. If a token is
  explicitly path-shaped (absolute/home/path separator), a missing/bad file is
  an argument error, not a collection fallback. Otherwise retain case-insensitive
  basename/stem lookup in accepted pool order, first matching entry wins (legacy
  per-entry filename-or-stem precedence). Document this deterministic ambiguity
  rule; no new selector syntax. Name lookup/random needs a usable pool.
- Structural config errors always fail. With rotation disabled, a bad/empty
  configured pool must not block an independently valid explicit file. Keep
  source-list/config diagnostics distinct from bad set-path arguments. A missing
  standalone config file uses defaults; a malformed existing file never does.
- Preserve config `path`/`list` trim-before-tilde expansion. Explicit CLI file
  arguments are literal filesystem arguments: do NOT trim legitimate leading/
  trailing filename spaces before actual-file lookup (supported tilde expansion
  still applies). Valid-empty list output and both/neither validation before
  manager/source setup are intentional corrections, not claims of byte-for-byte
  legacy control flow. Both/neither text/exit above and grammar exit 2 remain.

### Routing and isolation

- `set` prefers a running same-canonical-config host and uses its accepted
  state, including last-good state after disk rejection/deletion. Resolve local
  explicit file arguments before sending; never reinterpret them relative to
  host CWD. The server must enforce config identity itself on every mutation.
  Host routing is gated on Part 03's compiled budget path, fake-time cold-job
  proof and documented implementation feasibility evidence. Use that bounded
  request budget and coherent server deadline, not the default five-second wait
  by assumption or a blanket timeout increase. Built-in command defaults remain.
- Only proven absence (`NotRunning`, before any mutation bytes sent) allows
  standalone fallback. A live different-config host, remote error, unknown
  method, timeout, malformed response, broken pipe or uncertain send fails
  without local application. Do not retry a mutation or automatically launch
  another host. Test restart TOCTOU, not merely an initial `core.status` probe.
- A desktop instance may be starting while no socket exists: pre-send NotRunning
  still permits one local set, and the host's later initial job may override it.
  Document this race rather than adding startup coordination or a job protocol.
  At most one mutation attempt; never fallback after uncertainty.
- Standalone set reads current selected config and uses the same injected
  processor/native coordinator, initializing only narrowly required AppKit on
  process main as compile-proved in Part 02. It starts no registry modules,
  tray/window, watcher, timer, socket listener or startup commands.
- `list` and `generate-all` remain local read/cache work using current config,
  irrespective of `enabled`; no IPC/rotation startup needed. Simple list reads
  metadata only, not image contents or native geometry. Generate-all enumerates
  one native geometry snapshot, performs bounded per-display processing and
  never calls any native setter. Cancellation stops admission and joins workers.

### Output and exit evidence

- Empty valid `generate-all` prints exactly `No wallpapers to generate.` plus
  newline and exits 0 without processing/setters. Malformed config or directory
  I/O still fails rather than becoming empty success.
- Preserve generate-all header/results/summary structure and wording from legacy
  `manager.rs`: `print_generation_header`, `print_generation_results`,
  `print_single_result` and `spawn_progress_reporter`. Preserve per-screen result
  grouping (historically zero-based headings), cache-hit/error indicators,
  timing/counts and TTY spinner/cursor cleanup. Pipeline-versioned cache names
  may differ; freeze fixture names/time deterministically rather than invent
  output. ANSI/spinner/cursor control is TTY-only, correcting legacy unconditional
  color in several helpers under the shipped foundation policy.
- Preserve **exit 0** for a completed generation batch with individual image
  failures, while reporting those failures and counts. Top-level config/setup,
  native-enumeration, cancellation and output failures fail; no silent change to
  the established partial-image-failure exit contract.
- Capture static and TTY golden fixtures from the authoritative source helpers
  using valid config and injected clock/progress/writers. Do not fabricate
  messages from archived docs or run legacy native actions to capture them.
  Check stdout/stderr separately, newline, status and ANSI stripping; retain
  existing cache clear/path and config CLI contracts as regression fixtures.

### New error adapters (frozen contract)

New domain errors print `stache: Wallpaper error: ` followed by one body below,
one trailing newline, stderr, exit 1; never print the success line. Values in
braces are substituted, not literal braces. Quote paths/identities with serde
JSON string escaping (including control characters, no raw ANSI); target lists
use compact JSON arrays in operation display-snapshot order. No JSON object is
stuffed into a message or parsed as control flow. Optional OS cause detail appends
` Detail: {cause}` before the newline, where cause is one escaped quoted string;
goldens inject fixed causes rather than arbitrary platform wording.

| Typed condition                                           | Body                                                                                                                                                                                         |
| --------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Different config                                          | `Running host uses config directory {host}; requested {selected}. Use the same config directory or stop that host.`                                                                          |
| No accepted config                                        | `No accepted Wallpapers configuration is available. Fix wallpapers.jsonc and reload, then retry.`                                                                                            |
| Queue full before admission                               | `Wallpaper queue is full; no native change was issued. Retry after current operations finish.`                                                                                               |
| Deadline before any native issuance                       | `Wallpaper deadline expired before native application; no native change was issued. Retry when ready.`                                                                                       |
| Cancelled before any native issuance                      | `Wallpaper operation cancelled before native application; no native change was issued.`                                                                                                      |
| Response/transport uncertainty                            | `Wallpaper outcome is unknown after a send or response failure. Check the desktop before retrying; no automatic retry was attempted.`                                                        |
| Partial/failure/cancelled/uncertain after native issuance | `Wallpaper application {outcome}: applied={applied}; failed={failed}; not-attempted={notAttempted}; unknown={unknown}. Check the desktop before retrying; no automatic retry was attempted.` |
| Other typed pre-native domain failure                     | `Wallpaper operation failed before native application ({category}): {detail}. No native change was issued.`                                                                                  |

`outcome` is the fixed lowercase tag `partial`, `failure`, `cancelled` or
`uncertain`; category is a fixed typed error-category tag and detail an escaped
quoted string. Render known identities without guessing missing targets; a lost
response has no invented empty identity lists. An actual I/O timeout can occur
after commit: conservatively use response uncertainty, never the zero-effect
deadline body unless a typed server outcome proves no native issuance. Part 03
owns the DTO schema; these strings are display adapters only, not a new protocol.

## Acceptance criteria

- [ ] WP04-01: Composed command tree/help/completions contain exactly the intended legacy
      wallpaper surface, with source-derived static/TTY output/status goldens.
- [ ] WP04-02: Enabled false and paused explicit set succeed without lifecycle/timer
      changes; list/generate-all remain local and never apply images.
- [ ] WP04-03: Same-dir routing uses Part 03's proven finite cold-request budget;
      server refusal and absent-only fallback pass tests, with zero duplicate
      local calls after any uncertain-response/send case.
- [ ] WP04-04: Actual file and collection name precedence, invocation-CWD resolution,
      malformed-source handling and pre-work screen checks are documented/tested.
- [ ] WP04-05: Completed partial-image generate-all exits 0 with accurate results;
      top-level failure/cancellation exits nonzero and cleans up resources.
- [ ] WP04-06: Shared generated Rust bindings/schema checks, crate/full gates, Rustdoc
      warnings denied and `bun run tauri:build` pass. Feature docs accurately
      distinguish shipped work from any unverified native observation.

## Tests

Use fake native/clock/processor services and disposable config/cache/runtime
roots in every automatic test. Cover full flag tree/aliases/defaults; both/
neither path argument errors; 0/1/main/N, bad/overflow index before processing;
explicit external path versus basename/stem collisions; config base versus CWD;
config whitespace/tilde versus literal filenames containing spaces;
missing/default versus malformed config; usable/empty/missing pools; partial
batch errors/cache hits/TTY cleanup; write failure; bounded cancellation.

Fake socket hosts cover same/different directories, aliases and absent directory
suffixes, initially malformed config fixed without restart, paused/disabled
staged reload, invalid/deleted last-good host config, absent/refused socket,
remote errors, dropped/late response, partial send, malformed response and host
restart between probe and mutation. Assert exactly one mutation attempt and
zero fallback after any uncertain outcome. Standalone composition tests assert
zero tray/window/listener/module/timer/startup side effects.
With fake time, exercise the Part 03 cold job beyond five seconds and timeout
while queued, before commit and after native issuance through CLI routing;
verify the bounded wait policy and zero fallback even when outcome is unknown.
Golden-test every new adapter with deterministic quoted paths/IDs, escaped ANSI/
control characters, partial/unknown identities and actual router/client DTO
roundtrips. Assert empty generate-all text/newline/exit and zero setters, grammar
exit 2 versus both/neither exit 1 before any manager/source work. Model socket-
absent startup followed by a host initial job overriding the lone local apply.

## Authorized manual verification

Only after explicit **each-run** approval: record macOS version, display identity,
pixel geometry/scales, active/inactive Spaces and separate-Spaces setting;
capture before/after desktop-image URLs per target. Exercise public native
per-display set, mixed HiDPI, primary targeting, pause/disable and cache-clear
behavior. Restoration also needs authorization and the captured exact original
URLs; it is never an automated test/CI cleanup action. Record which Spaces
actually changed and what was not testable. Do not claim universal Space
coverage, lasting OS retention of deleted cache URLs or rollback of partial
application. A declined manual run remains a visible verification gap.
