# Foundation validation

Read when running checks or verifying tooling isolation.

Use Bun 1.4.4, Node 26, the root Rust nightly and macOS 26+ with Xcode command-line tools.
Install cargo-sort 2.1.4, cargo-nextest 0.9.151 and (for audits) cargo-audit 0.22.2, the versions CI
pins. CI uses macOS 26.

```sh
bun install --frozen-lockfile
node_modules/.bin/playwright install webkit
bun run lint
bun run format:check
bun run test            # test:ui + test:e2e + test:scripts + test:native
bun run test:ui:coverage
bun run test:e2e        # Playwright scenarios only
bun run build
bun run generate-schema
bun run generate-schema -- --check
bun run tauri:build
```

## Tiling borders

`stache-macos/src/border_overlay/` and Tiling's `borders/` are always built and run in the default suite
(`cargo nextest run --workspace --features stache-app/dev-tools`; there is no borders feature flag). The
overlay tests use `FakeOverlayBackend` (the real owner-map and pool logic over a host that
only records native calls) with a queued `ManualExecutor`; no panel, layer, display link, `dlsym` or
`SkyLight` connection is created. Appearance (`borders/appearance.rs`: color parsing, state precedence,
placement, style and the overlay mapping), the pure model and motion have unit tests.
Tiling's border scenarios (`modules/tiling/src/runtime_tests/borders.rs`) drive the real actor, engine,
route and effect runner over the fake window and application backends and the fake overlay, and read one
ordered log that interleaves overlay commands with native hide, unhide and frame-write calls.
Spaces and Mission Control (part 05) use `FakeSpaceEvents` (scriptable enter, exit, duplicate,
out-of-order, overflow and relaunch facts; no observer, symbol or thread) and a switchable
active-Space confirmation; the pure gate (`borders/spaces.rs`) needs neither.
Lifecycle (part 07) tests in `module_tests/native.rs` run the real registry and actor over a `ManualExecutor`
whose queue the test runs (or a test-owned pump thread), scriptable stuck owners (`hold_stop`) and a
recording release probe; the quit-time defect has a regression there with a main thread that never
runs the release job.
The native sources (`border_overlay/space_sources/`) are tested over fake `SpaceSession`s, display
listings and on-screen window lists (notification-to-fact mapping, Dock relaunch, resync, stop closing
the sink before the join, the join-bound outcome, a missing private symbol); the default tests register
no observer, resolve no symbol, read no window list and create no window or thread beyond the fake owner.
Live follow (part 06, `border_overlay/live_follow/`) is tested over `FakeSessionControl` (a fake
notification session: injected move, resize, reorder, hide, unhide and close for own, foreign and unknown
ids, a scripted bounds read, a failing registration or subscription), `FakeFrameClockFactory` (manual ticks,
held callbacks, stuck stops), `FakeSink` and `FakeLiveFollow`; no symbol is resolved, no display link or
callback is created. The public fallback is tested with an unavailable `FakeLiveFollow`.
Native border checks (one display, the user's own configuration) each need per-run approval and never
run from the default suite; their method and results are recorded in the notes of the
[borders spec](../specs/2026-11-09-tiling-borders/00-overview.md) parts, and what stays unverified is
listed in [Tiling borders](../features/tiling.md#borders).

## CLI and socket checks

`cargo nextest run -p stache-cli -p stache-sdk` covers the standalone commands,
the wire protocol, endpoint safety and the socket limits without touching a
device. The app integration tests run the real `target/debug/stache` binary
with an injected `HOME`/`TMPDIR`: a command that initialized Tauri or the tray
would not exit, and the bounded wait fails. They also exercise piped and PTY
output (`script -q /dev/null`) and the exit codes 0/1/2.

`stache audio list` is the only command that reads real `CoreAudio`. It is
read-only: it enumerates devices, never sets a default, never registers a
listener and never starts the desktop. Tests use its fake backend for goldens
and run the real binary only through that read-only path. Never enable
`proxyAudio` for a test, a smoke run or a CLI check.

No automated check may force-terminate Apple Music/iTunes or open a replacement
player. The noTunes opt-in check runs only with approval for that run, from an
isolated temporary config/identity with any legacy noTunes disabled; see
[notunes.md](../features/notunes.md).

Keep Awake validation is fake-backed. No automated check creates a real power
assertion, locks or unlocks the screen, wakes the machine, installs a real
distributed observer or launches Stache. Module tests drive fake assertion
and observation backends, in-memory lock results, barriers and races, and a fake
clock; app host tests write malformed TOML, a wrong boolean and a deleted file,
run the normal watcher/reload path, query the actual `keepAwake.status` socket,
restore identical last-good bytes and verify both held store and snapshot
diagnostics clear without lease churn. They also cover tray Enable/Disable TOML
edits, session Pause/Resume without disk writes and shutdown cancellation. The host
suite drives the real coordinator over the fake owner: a clean module stop with
observation removal ends `Clean`, a persistent release failure ends `Partial` naming
`module:keepAwake` and `power-assertion`, and a barrier-hung release or observation
removal ends `Uncertain` without a false clean. The Keep Awake envelope and
runtime DTOs use the shared bindings collector, so the generated DTO check is
their gate; there is no Keep Awake Tauri command or CLI command. Real
three-assertion behavior, notification delivery, manual lock/unlock, resume and
forced-sleep checks remain **unverified**; they require approval for each run and
the [spec's power protocol](../specs/2026-11-03-keep-awake/00-overview.md#validation-and-safety).

Bar's backend (part 03: `stache-bar` `tests.rs`, `window.rs` and `stache-app` `bar_tests.rs`) is tested with fakes only. The unit tests run the real `BarModule` in a real `Registry` over a fake window backend that reports phases like the host (async created/loaded, close acknowledged later) and a scripted topology feed that applies to the registry: frame math (negative origin, scale 2, a primary that is neither first by position nor `mainScreen`, rounding, zero-size and over-limit), zero-size transitions, accepted/rejected/echo reconfigure, create failure and load timeout after `start` returned (Bar stays `Running`, `panel: failed`, reservation cleared, feed kept), the bounded 3-step backoff retry, recovery when the host starts loading, convergence-trigger and pause/resume immediate retry, a stop and a shutdown during a pending load wait with a late load resurrecting nothing, stale-generation retry, no-primary and primary swap, a late close acknowledgement against a successor and a stuck feed end retried. The app scenarios (`bar_tests.rs`) build the real `Registry`, `ConfigStore`, `BarFactory`, the real topology feed over `FakeDisplayBackend` and the real `NativeWindows` over a fake `WindowHost` (a dedicated fake "main" thread that a test can stall), with the user's setup (`bar.toml` 28/14, a built-in primary 1512x982 at scale 2 and a left external 2560x1440 at a negative x) and a consumer that reads reservations through Tiling's own `HostReservations`/`usable_area`: startup (both displays Present, 42 on the primary, 56 usable top with a 14 gap), disabled and paused (no host topology, no window, no observer), hotplug (unplug, replug under the same and a reused id, a third display, primary swap: never `Absent`), reconfigure to zero height and back in place, a page that never loads / native create failure / page failure (Bar stays `Running`, Tiling sees `Present` with zero inset on every display, the exact window closed), backoff retry recovery and its bound, immediate retry on pause/resume, config change and hotplug, `start_all` plus a second module's pause/enable and a permission change all completing in under 2 s while Bar's page never loads, a stop during the pending load, permission revocation, a hung display query, a never-acknowledged close and a stalled main queue at stop. No real panel, display, process or network is involved. The real `NSPanel` hosting and menu-bar auto-hide were observed in the recorded live runs on macOS 27.0 (see [Bar live verification](../features/bar.md#live-verification)); real display hotplug (`NSScreen` notification delivery) was not exercised live, and every live run needs per-run approval.

Bar slice (c), the menu bar and the data bridge, is fake-backed as well. `stache_macos::menu_bar` tests (`menu_bar/tests.rs`) cover the pure resolution (either signal visible wins, undecided falls back to the layer-25 list then the prior then unknown, the legacy heuristic, the pointer row needing the right screen), when polling is needed, inline versus a never-draining main queue with a bounded budget, pure ordered hints, a late notification after stop and removal failure. `stache-bar` `menubar.rs` tests the pure `next_wake` schedule and observation dedupe; `tests.rs` runs the real runtime over `FakeMenuBarBackend`: no observer, worker or sample without a panel (disabled, paused, zero size, no primary), subscribe before the first sample, 20 rapid signal toggles with a constant `height + padding` reservation and no registry update, a dropped signal reconciled by a poll only while auto-hide needs it, the bounded poll cadence (about 8 samples in 400 ms at 50 ms, zero once auto-hide is off), burst coalescing, a deduplicated unknown that recovers, a failed registration degrading to a labelled poll, a stuck removal `cleanupPending` and retried with late callbacks ignored, zero-size leave and re-enter, and a stalled main queue bounding `end`. `stache-app` `bar_tests.rs` covers the bridge: authorization (live label, test/access/shell/forged/stale labels), typed `unavailable` reasons per state and on registry revocation, a stalled main queue or never-loading page never blocking `bar_snapshot` (under 200 ms for the queue, under 2 s for the page), DTO serde with exact strings above 2^53, the label shape and `sequence_is_newer` edge values, events with increasing sequences only to the Bar label, a failed emission and a coalescing overflow becoming `resnapshot`, shutdown during changes and before the subscription, an unattached bridge, and an end-to-end scenario (28/14 on two displays, menu toggles with increasing sequences while Tiling keeps a constant top 42, pause gives `unavailable` with zero observers and an idle bridge worker, resume recovers with a new generation). Auto-hide hiding and revealing the bar was observed live (part 04 run, macOS 27.0); which path delivered it (`NSMenu`/`NSEvent`/workspace notifications or the layer-25 window list) was not isolated.

Bar part 05 slice (b), the fixed click actions, is fake-backed. `app/src/bar_actions_tests.rs` runs the real `ApplicationControl` over `FakeApplicationBackend` (one thread per job) and a scripted settings opener: the fixed target per action, inline and deferred completion, completion failure, missing app (`notInstalled`), rejected URL and failed open (`refused`), an issued open with no completion (`uncertainAfterIssuance`, issued once, no retry), an acknowledgement timeout before issuance (the abandoned job never sends) and after issuance (`block_next_open_after_issue`), a stop during preflight and before the native send (the permit refuses; no successor effects), a pane guard failing at the send, 100 rapid clicks while one is in flight all `busy`, and the exact serde wire forms with non-enum input rejected. `access.rs` tests the pane opener (guard at the send, rejected URL, never-started job `timedOutBeforeIssuance` and never sending late, send outliving the bound). `bar_tests.rs` adds end-to-end scenarios (bar.toml 28/14): a click on each component through the bridge reaches the fakes with the fixed target, typed `notInstalled`/`refused`, `unauthorized` for test/access/shell/forged/stale labels, `unavailable` while disabled, paused or failed, stopping mid-click sends nothing, and a hung adapter or stalled queue never blocks bridge reads (under 2 s for the adapter, under 200 ms for the queue) or shutdown. `host_policy.rs` asserts the capability is exactly `allow-bar-snapshot`, `allow-bar-action`, `allow-bar-weather`, `core:event:allow-listen`, `core:event:allow-unlisten`. The WebKit suites of the previous UI were deleted on `feat/ui-rebuild`. No test launches an application, opens a URL or touches `NSWorkspace`.

Bar part 06 slice A, the weather location adapter, is fake-backed. `stache_macos::location` tests (`location/tests.rs`) cover the pure policy (authorization status to request/wait/proceed/fail with no re-request after a decided status, the temporary full accuracy conditions, `CLError` mapping, fix validation), coordinate redaction in `Debug`/`Display`, first-writer-wins completion, the generation gate dropping late events, `FakeLocationSource` recording the requested accuracy, and the real source stopping at `NoUsageDescription` in the test binary. The late-consent watcher is tested through the pure `ConsentWatch` state machine (timeout while awaiting keeps one watcher, a decision notifies once and ends it, cancel or a newer generation drops late decisions, never two managers) and `FakeLocationSource::{decide_consent, watcher_pending, watchers_created, watchers_cancelled, notifications}`; `stache-bar` weather tests assert the cache drop, a single `weatherEpoch` bump, a decision after stop bumping nothing, and the app `bar_tests.rs` scenarios assert the `BarChanged` with the bumped epoch, the located second `bar_weather`, and no event or manager after stop. The UI tests assert one refetch per epoch change, none for an unchanged epoch, and that an older in-flight answer cannot replace a newer one. No test constructs a `CLLocationManager`, prompts or reads the real location. Config tests cover `precise_location` decode, default, wrong type and migration, and `refresh_interval` default 20, the 5 and 1440 bounds, rejection of 4, 1441, 0, negatives, floats and strings at `weather.refresh_interval`, migration, schema minimum/maximum and the template line; service, bridge and UI (`Weather.query.test.ts`) tests assert `refreshIntervalMinutes` reaches the answer, a reconfigure changes it and polling follows the configured minutes. `src/crates/app/Info.plist` is embedded in debug builds by `tauri-codegen` (check with `otool -s __TEXT __info_plist target/debug/stache`); `host_policy.rs` asserts `bundle.macOS.entitlements` points at `Entitlements.plist` holding only the location entitlement and that `Info.plist` has the usage string. `release_metadata.rs` checks that root `Cargo.toml`, `package.json` and `tauri.conf.json` declare one version, that `Info.plist` holds exactly `LSUIElement`, `NSHighResolutionCapable`, `LSApplicationCategoryType` and the two location keys, that every file in `icons/` is in `bundle.icon` (or is `icon.png`), and that capabilities, `bundle.targets`, `bundle.resources` and entitlements stay unchanged. The consent prompt only appears for a bundled `.app` (`bun run tauri build --debug --bundles app`, launched with `open`); the dev binary never prompts. Live consent checks need approval per run.

Bar part 06 slice B, the weather data path (`bar_weather`), is fake-backed and reads no real location, home or environment. `stache-bar` `weather/tests.rs` (child of the crate `tests` module, so it reuses the real-registry harness): env parser fixtures (quoted, single-quoted, comments, `export`, whitespace, CRLF, `=` in values, missing/empty/blank key, malformed lines, last duplicate wins, unterminated quote, substitution from earlier entries only, BOM, invalid UTF-8), the file reader over temp directories with an injected home and config directory (empty `api_keys` reads nothing, relative against the config directory and never the working directory, absolute, `~` with and without a home, missing, directory, oversized and exactly-at-cap, unreadable, symlink, key absent, redacted `Debug`), the exact serde wire forms, and the resolver over `FakeLocationSource` and a gated source: a fix, a reduced fix, every `LocationUnavailable` variant falling back with its typed diagnostic, an empty default location giving `none`, `precise_location` reaching the recorded request and a reconfigure applying at the next read, 60 s reuse on a fake clock (59 s reuses, 60 s reads), a new revision or restart reading again, no read while disabled, zero-size, paused or failed, four concurrent calls sharing one read, a hung read timing joiners out without a second read, stale after a revision change or a stop during the read with nothing recorded, and a weather note changing no sequence. `stache-app` `bar_tests.rs` adds end-to-end scenarios (28/14, two displays, `[weather]` with an env file and a default location, Tiling top 42): `bar_weather` through the real bridge returns the key and coordinates without touching the view, denied/undetermined/no-usage-description fall back, `precise_location` applies without a restart, `unavailable` with zero recorded reads for disabled, paused, gated, failed and zero-size, `unauthorized` for test/access/shell/forged/stale labels, stop during a blocked read is `stale` with no later effect, a hung read never blocks `bar_snapshot` (under 2 s) or shutdown, a missing key file is a redacted diagnostic and Open Meteo reads no key, and a sentinel key and sentinel coordinates never appear in the serialized `BarView`, `BarChanged`, `BarRuntimeStatus`, `ModuleStatusView`, `StatusChanged`, any `Debug` output or captured logs (with the response as the positive control). The weather click is covered like the other actions in `bar_actions_tests.rs` and the click scenarios. UI code, providers and CSP tests are slice C (next paragraph).

Bar part 07 slice B, the media provider, is fake-backed: no adapter, perl, real child, real clock sleep or user cache directory is touched. `stache-bar` `providers/media/tests.rs` uses a scripted `ChildProcess`/`ProcessFactory`, a virtual clock and a temp cache directory. Parser: empty payload, missing title, full mapping, unknown keys, wrong-typed fields, character caps, invalid wrapper/type/payload shapes, nesting past the recursion limit, artwork pairing. Artwork: every MIME fixture generated in-test with the `image` crate (center crop and deterministic pixels, solid-color exactness, portrait crop), stable content id, placeholder/MIME/base64 rejection, the encoded cap before decoding, dimension and pixel caps rejected from a header-patched PNG before the pixel decode. Store: round trip as base64 `.txt` with `0600`/`0700`, memory-only, traversal-proof ids, symlinked entry and directory refusal, corrupt/oversized/mismatched entries, pruning to 64 entries, bounded memory layer. Publisher: duplicate coalescing, stale artwork never attaching to a newer state, nothing published after close. Owner: exact command line, state then artwork id, coalescing, whole-state replacement, split UTF-8 and multi-line chunks, oversized/invalid frames, backoff schedule 1, 2, 5, 10, 30, 60, 60 s on the fake clock, reset after a 60 s run, restarting then unavailable (media hidden), cancel during backoff waking at once, stop ordering (media invalidated before SIGTERM), SIGTERM ignored then SIGKILL, an unreapable child staying retained with no second spawn, bounded sanitized stderr flood, unavailable or relative resources spawning nothing, a stop before the first spawn. `stache-app` `bar_actions_tests.rs` covers the media click map (four players, look-alikes inert, not installed, stale) and `bar_tests.rs` adds end-to-end scenarios (bar.toml 28/14, two displays, Tiling top 42): a Spotify track with artwork reaching `BarView.media` and `bar_media_artwork` with the wire form pinned and the artwork bytes absent from the view, `BarChanged`, status, module view, `Debug` and logs; stop/pause reaping the exact child, hiding media with no late publish, and a resumed successor inheriting nothing; crash then recovery; repeated crashes reporting `unavailable` while Bar stays Running; unavailable resources; disabled, zero-size and permission-revoked Bars with zero children; a click for each known player and inert unknown/absent media; artwork authorization for the live Accessibility guide window, forged test/access/shell labels, forged and stale Bar labels and malformed ids; production parts using `<cache>/media-artwork` and the vendored resources. `host_policy.rs` now expects `allow-bar-media-artwork`. Live streaming was observed only in the approved dev run (Spotify, macOS 27.0, which counts for the macOS 26+ gate by user decision 2026-11-10); the adapter from the bundled copy and artwork under the real CSP in a bundle stay unverified.

The Accessibility guide (migration part 07) is covered by `windows/AccessibilityGuide/**/*.test.ts(x)` (every section, the unbundled variant, the error line, self-close on `open`, each command sent once and disabled while pending, Escape without auto-repeat and Close through a mocked current window, the drag region, the calls made), with `e2e/scenarios/access.spec.ts` and the whole-session `bar-session.spec.ts`. `windows/Bar/widgets/registry.test.ts` proves the registry is exhaustive at the type level.

Bar part 06 slice C (the weather UI) and Bar part 04 slice (b) (the static surface) were tested by WebKit suites of the previous UI; their replacements are the UI suites below. The rebuilt weather UI (migration part 04) is covered by `windows/Bar/widgets/Weather/*.test.ts(x)` (providers and location with a stubbed `fetch`, fake-timer timeout and interval tests, abort, key contents, icon tables, display states and click) and `e2e/scenarios/weather.spec.ts` (fake host plus Playwright routes for the provider hosts; any other remote request is aborted and asserted absent). The Rust fakes of part 04 slice (b) remain under `test:native`. The rebuilt Media UI (migration part 05) is covered by `windows/Bar/widgets/Media/*.test.ts(x)` (label, icon table, artwork query outcomes and id changes against the mocked IPC, retained artwork with real decoding of the valid and invalid PNG fixtures in `tests/png.ts`) and `e2e/scenarios/media.spec.ts`. The rebuilt Spaces UI (migration part 06) is covered by `windows/Bar/widgets/Spaces/**/*.test.ts(x)` (states, Tiling's published order, labels, compact mode, clicks, every icon-table entry rendered, motion with animations on and under reduced motion, and a render-count test) and `e2e/scenarios/spaces.spec.ts` (a whole session through the fake host).

Keep Awake published state (Bar part 08 slice A): the keep-awake suite decodes the channel through the real registry for every transition (schema identity, held start publication, tombstones on pause/disable/failed release, sequence restart on a new generation) over the existing fakes; `app/tests/keep_awake_status.rs` pins the `keepAwake.status` bytes with a golden captured before the change; `published_state.rs` proves codec registration, mismatched-version and wrong-provider rejection and an independent decode; `bar_keep_awake.rs` table-tests the projection (`Tracker`) for initial null, active only when confirmed, every other state, late old-generation writes/tombstones and sequence reset; `bar_tests.rs` runs the real Keep Awake factory on fakes under the 28/14 two-display Bar (follows adoption/release, pause/disable facts, absent/disabled/failed Keep Awake, rejected-file retention, watcher lifecycle). The reload-based file cases use `ConfigStore::reload_one`, the same path the watcher calls. No test creates a real assertion or changes the lock state.

Keep Awake session click (Bar part 08 slice B) is fake-backed; no real assertion or lock change. `session_action_tests.rs` pins the exact serde wire forms of every outcome, the closed `pause | resume` enum (`enable`/`disable`/`retry` do not deserialize), the Bar capability policy (`keepAwake` Pause/Resume only) and the job waiter (finish, shutdown cancelling a queued job, `uncertain` for a started one, bounded timeouts). `tests/keep_awake_host.rs` runs `HostHandle::session_action` on the real host runner over the Keep Awake fakes: pause/resume applied with the host status sink seeing the change and zero configuration writes, a disabled module inert (zero writes, no native activity), an external tray pause racing a click never inverted, a failed module typed `unavailable` with no acquisition, a second click while one is in flight `busy`, and a shutdown during a click (the release parked) returning `uncertain` without hanging. `bar_tests.rs` (28/14, two displays, Tiling 14/10) clicks through the real bridge: pause then resume with the pushed `BarChanged` (`{active:false,paused:true}` then active), running-but-locked, disabled/absent inert, failed, release pending (no retry), repeated and external-pause stale clicks, authorization of every invalid label, module and the stale label, Bar not running, registry shutdown, and a watcher with zero idle wake-ups that wakes through the host status sink. `tests/bindings.rs` pins the generated `barSessionAction` command and `host_policy.rs` the exact capability set. The WebKit suites of the previous UI were deleted on `feat/ui-rebuild`.

Tiling–Bar integration (part 10) is fake-backed; no real window, AX call, activation or panel. `bar_tiling_tests.rs` table-tests the projection and the `bar_tiling_action` validation. `src/crates/app/src/bar_tiling_e2e.rs` (a child module of `bar_tests.rs`) runs the **real** Tiling module (`TilingFactory`, its actor and its `TilingFocus` path) next to the real Bar in one registry over `FakeWindowBackend` (echoed frame writes), `FakeApplicationBackend`, a two-display fake (built-in 1512x982 plus a 2560x1440 external, menu bar auto-hidden) and the fake panel host, with `bar.toml` 28/14 and `tiling.toml` gaps 14/10. It asserts Tiling's planned frames and the `BarView.tiling` / `BarChanged` the UI receives: the strip contents and click round trips, minimized/hidden windows and silent frame-only activity, loading before the first publication, paused/disabled/failed Tiling as `absent` while Bar and Keep Awake keep updating, the reservation lifecycle (constant while slid hidden, held while loading, released by a failed panel, exact-generation tombstones, stale releases), click authorization and revalidation, busy and stuck clicks ending `uncertain` at shutdown, the watcher lifecycle, and the final integration gates: rejected `tiling.toml` files keeping the strip and revision with ordered A–B–A accepted edits, inert late clicks and writes after shutdown, Hold-to-Quit > MenuAnywhere > Keybindings at the host priorities over a fake shared input service with Tiling listen-only, noTunes' separate application observation, and Keep Awake disable/enable plus Bar pause/resume recovering the strip with a strictly increasing `BarChanged` sequence. Native clicks, focus and live geometry stay **unverified**.

Published state and screen reservations (`stache_sdk` `published_state/tests.rs`,
`screen_reservations/tests.rs`) use scripted fake modules (`on_start`, `on_reconfigure`,
`on_stop`) and channels as barriers instead of sleeps: a blocked `reconfigure` plays the
window where a worker still computes with the old config, `StateCell::set_before_wait` /
`ReservationHub::set_before_wait` land a write between the wait check and the sleep, and
`StateCell::set_after_advance` runs between the cell learning a revision and the accepted watch
showing it. Tests assert that envelope revisions are the provider's label, that unaccepted or older
revisions are rejected, that a failed reconfigure changes nothing, that never-started providers read
their registry status, that stops by disable, pause, permission revocation (`on_permission_change`)
and dependency loss (`service_failed`) remove exactly the ending generation's reservations, and
that repeated capability requests keep one end listener.

The shared input contract (`stache_sdk::input`) is tested only with fakes: a fake
clock that advances instead of sleeping, a fake native owner over the real
256-request control queue (acknowledge, fail, hold, delay, drop or reject
per request), a capacity-limited work fake, and recording handlers/observers.
Tests use barriers for simultaneous subscribe/unsubscribe, drive fake time for
the 2 s acknowledgement, 10 ms quantum and over-1 ms callbacks, and exercise the
exact 1,024 ledger and 256 queue boundaries. No test creates a real event tap or
hotkey. The `stache-macos` owner (`input/`) is tested the same way: a fake native
layer behind the owner's small `NativeInput` trait drives the real queue, thread,
acknowledgement, cancellation and shutdown logic (cancelled queued install never
mutates, partial install cleaned by remove, removal failure reported, bounded
shutdown, all native calls on the named owner thread, no main-thread dispatch), and
event conversion is a pure function over plain field values. The real
CoreGraphics layer is compile-checked only (it must stay `!Send`). Live tap
behavior, Input Monitoring/Accessibility prompts and a real event stream stay
**unverified**; any manual run needs approval for that run and a cleanup record.

The Carbon hotkey adapter (`stache-macos` `hotkeys/`, `keyboard_layout/`) is tested only with a
fake Carbon/TIS layer: no test registers a real hotkey, installs a real Carbon handler or
distributed observer, reads or changes the real keyboard layout, or touches a permission. A
`ManualExecutor` plays the main thread (inline, or queued so a test interleaves a hint, a stop
and a drain), and the fake records every native call, scripts an occupied chord, handler-install,
unregister and observer failures, switches layouts mid-commit and delivers synthetic Carbon events.
Tests cover per-chord outcomes, staging reuse and rollback, stale layout and generation rejection,
coalesced refresh hints, queued stale events after ID replacement, central reservation conflicts
and yield/restore reflected in dispatch, cleanup failure with retained inert guards, and the
2 s / 10 ms acknowledgement bounds (short real budgets, never a real wait). Real registration,
real layout switching and real input-source notifications stay **unverified**; a manual run
needs approval for that run, a harmless owned command and a confirmed unregister.

Keybindings runtime tests (`stache-keybindings` `tests.rs`, `dispatch/tests.rs`, `status.rs` and the
`host.rs` Keybindings tests) use the `testing` feature's `test_support::Rig`: a pump thread plays the
main thread (so the thread-local Carbon store and fake handler behave as on main), the Carbon layer is
`FakeLog`, the tap is the fake input backend over the dispatcher's own reservations, `hidutil`, the
journal and the lease are the Caps `World`, children are a scripted process factory and time is fake.
Barriers hold the coordinator between a publication and its adoption (`RuntimeTiming::observed`) to
prove no press runs in that window; others block the pump to prove stop and status reads are bounded.
They run the real `Registry`, `ConfigStore` and `Runner` registration/start path. Nothing registers a
real hotkey, installs a real tap, runs `hidutil` or spawns a real command; real shortcut, layout, Caps
and process-lifecycle checks are manual and need approval for each run.

Hold-to-Quit tests (`stache-hold-to-quit` `tests.rs`, plus the Hold-to-Quit host cases in the app `host.rs`
suite) use the `testing` feature's `test_support::Rig`/`Fakes`: the shared `InputService` over its fake
tap and clock (callbacks are driven with `InputService::dispatch`), `FakeApplicationBackend` with
`FakeBarrier`s at the preflight, commit, issuance and after-issue points, `FakeHudBackend`, a separate
HUD `ManualExecutor` that can queue or never drain, a `PumpMain` thread for tests that need a real
cross-thread acknowledgement wait, and `FakeHoldClock`, whose `park` records every requested wait and
whose `advance` wakes the worker only when its timeout is due (so an idle worker is proven to request
one indefinite wait and nothing else). They cover the modifier matrix, both frontmost gates, inbox
overflow, slow and panicking callbacks, release before/at/after the deadline and the commit,
`Uncertain` versus `Refused`, focus changes and bursts, reconfigure rejection and the
reconfigure-to-publication window (`HoldToQuitRuntime::set_reconfigure_hook`), retained HUD and
observer cleanup, and the Carbon yield in both registration orders. Nothing installs a real tap, shows
a real panel, terminates an application or prompts. Real tap, HUD, display, Space and quit checks are
manual and need approval for each run (see [the feature](../features/hold-to-quit.md#verification)).

MenuAnywhere tests (`stache-menu-anywhere` `tests.rs`, plus the MenuAnywhere host cases in the app `host.rs`
suite and `config_catalog.rs`) use the `testing` feature's `test_support::Rig`/`Fakes`: the shared
`InputService` over its fake tap (callbacks are driven with `InputService::dispatch`, including
competing handlers at other priorities and `tap_disabled`/`retry_reenable`), `FakeApplicationBackend`
with `FakeBarrier`s, the **real** `MenuControl`/`AxOwner` over the Part 02 fakes (`FakeAx` with a hook
that parks a read or an `AXPress`, `FakePresenter`, `FakeGeometry`, `FakeMainThread` that can pause or
reject) and the fake `FakeClock`. `Rig::hold(Stage)` parks the worker or the config worker at
admission, capture, read, show, action, cleanup or just before the config worker publishes the tuple. Cases
prove the prefilter shape, trigger tables, event-point anchoring, admission and coalescing, acceptance coherence across reload/pause/disable,
capture timing and staleness, rollback of a failed start, stop with an open menu or a stalled press
or read, pending cleanup that never touches a successor, and that only the explicit tray request
reaches the permission provider's `request`. Nothing installs a real tap, reads a real application,
shows a real menu, activates, presses or prompts; every blocking wait has a timeout. Real tap,
Accessibility, `NSMenu`, multi-display and quit checks are manual, need approval for each run and are
**not yet performed** (see [the feature](../features/menu-anywhere.md#verification)).

Tiling's pure code (`stache-tiling` `geometry`, `layout/`, `rules`, `presets`, `minimum_size`; fixtures in
`pure_tests/`) has no native, thread or actor dependency, so `cargo nextest run -p stache-tiling` runs it
everywhere. Golden geometry (portrait/landscape, scale 1 and 2 as logical-point linearity, negative
origins, all four insets, the top-46 example, Grid 0-10/11/1000) is derived by hand from the cited
legacy algorithms, never pasted from this crate's output, and legacy code is never run. Rules are
checked prepared against unprepared and against an independent reference over a Unicode corpus.
Fixed-size and refusing windows are plain `observe_write` inputs; no application is resized.

Tiling's native adapters in `stache-macos` (neutral `ax`, application hide/unhide and lifecycle,
`displays`) are consumed by the Tiling runtime and are tested only with fakes and local pure values:
`ax::testing::FakeWindowAx` (scripted missing, malformed, timed-out and partially written
attributes; recorded call order and element balance), an `AXValue` round trip through the real
`AXValueCreate`/`AXValueGetValue`, the `AxObserver` callback body driven with locally created
`AXUIElementCreateApplication` elements (a local constructor that sends no message), an injected
`_AXUIElementGetWindow` symbol lookup, `FakeApplicationBackend` hide/unhide barriers and lifecycle
deliveries, and `displays::testing::FakeDisplayBackend`. No test queries a real application,
registers an `AXObserver`, hides or unhides an application, posts or observes a real workspace or
screen-parameter notification, or reads `NSScreen`. There is no new manual gate in this slice.
Real `AXObserver` delivery, `AXUIElementSetAttributeValue` on a window, the private symbol, hide/unhide
effects, lifecycle notification delivery and reconfiguration hints remain **unverified** and need
approval for each manual run.

Native tabs (`tabs.rs`, `state/tab_sync.rs`) are tested with fakes only: `FakeWindowAx` tab strips (tab elements with ids, selected flags, failures; `read_tab_group`), the real owner `Core` with tab reads on (`windows::tests`, `runtime_tests::real_core`), and pure transitions in `state/tab_tests.rs` and `tabs.rs` (multi-tab app, switching, tab-only create/destroy without a new plan or write, group deletion, reused ids in another application or group, unreliable strips, failed reads, reads off, initial scan once). No test queries a real tab strip. Tab bar continuity (the shared `AXTabGroup` identity) is tested the same way: `FakeWindowAx` tab groups are nodes that several windows list as a child, so a group moves between windows by editing `children`; `windows/tests/tab_bars.rs` drives the real owner `Core` (equal group elements share an id, different bars and plain windows do not, a bounded nested search, a switch and a created tab keeping the id, a window whose id and bar are unreadable neither adopted nor destroyed and no serial consumed, a cached bar for geometry reads, orphaned bars ageing out, no leaked element after untrack or stop), `runtime_tests/tabs.rs` drives the actor with a `ManualClock` (switch, new tab, close with a transient listing, bounded settle retries, last tab closed, Ghostty-like single window, separate groups, frame-sharing windows never merged, preserved workspace/floating/focus, one coalesced listing per burst, the old token staying dead, a Finder `AXDialog`), and `runtime_tests::real_core` runs a switch and a close through the real owner core. Suspended windows (a window that left `AXWindows` while its element lives) are tested the same way: `FakeWindowAx::kill` makes a node answer like a dead element, `windows/tests/suspended.rs` drives the real owner core (a live unlisted window suspended once and kept with its registrations, a dead or re-id'd one retired, a destroy of a suspended window, a suspension reported before the creation of its replacement, a return announced by a creation, the bound of 32, an aborted listing deciding nothing, no leaked element after untrack or stop), and `runtime_tests/tabs.rs` models the Ghostty probe (Command-T and Command-W on a single window three times with no extra tile, reflow or write; three-tab switching returning windows by identity; closing an inactive tab changing nothing; closing the active tab handing the slot to the listed suspended member; the last window reflowing; two suspensions with one tabbed arrival not matched and released after the settle; a return announced before the close waiting for the settle). No test queries a real tab bar or a real Ghostty window.

Slice 03b (`stache_macos::windows`, consumed by the Tiling runtime) is tested with fakes: `FakeWindowSystem` (a
`FakeWindowAx` arena plus scripted `FakeObservers`: injected notifications, overflow, add/remove
errors, removal-attempt counts), a stepping clock, a recording `ScriptedDriver`, and real owner
threads that run a plain `CFRunLoop` over the fake system. The fake suite covers 0.5 s query and 1 s
action timeouts, the 2 s hop and 10 ms cancellation quantum, enumeration yielding at 64 elements and
50 ms, pre-issue expiry versus in-flight overrun (`Uncertain`), no idle duration timers, bounded sink
overflow, duplicate and reused `CGWindowID`s, application/PID replacement, two adapters and epochs,
destroy/recreate, delayed callbacks, lost acknowledgements, malformed values, partial position/size,
rejected dispatch, a stalled native send with concurrent invalidation, permission loss, cleanup retry,
quarantine blocking reinstall and distinct thread names, refresh reporting of live windows and retirement of closed ones (and none on an aborted or capped listing), focus-before-create ordering in one drain (deferred focus keeps notification order, latest wins), a stale backlog dropped on untrack and re-track, keys kept while an undrained event names them, burst creations yielding per the element and time budgets, budgeted retirement of missed windows with cancel/deadline/seal aborts, and request cancellation within the quantum. The applications fake models intern-time pruning with a bounded tombstone list, so a terminate after another application's intern still names the original id and an evicted one is `TerminatedUnresolved`. Panic tests are labelled **unwind-only**:
the release profile aborts, so they are not release recovery evidence.

| Capability (TI-03-04)                                           | Proven by compile/fake                         | Unverified natively                                                     |
| --------------------------------------------------------------- | ---------------------------------------------- | ----------------------------------------------------------------------- |
| Owner confinement, `!Send` elements and observers, no main wait | compile-time assertions, owner-thread tests    | behavior under a hung real application                                  |
| Token revalidation, destroy-before-queued, `CGWindowID` reuse   | fake owner tests                               | real `AXWindows` membership and `CFEqual` behavior                      |
| Gate, one-shot commit, lock-free invalidation, `Uncertain`      | stalled-send barrier tests                     | real send latency and `AXError` mapping                                 |
| Position/size write, `AXRaise`, read-back                       | `FakeWindowAx` call order                      | real windows accepting or clamping frames; `AXRaise` without activation |
| `AXObserver` delivery on the owner run loop                     | mailbox purity tests; plain run loop wake test | real notification delivery, ordering, destroy semantics                 |
| Run-loop wakeup (`PerformBlock` + `WakeUp`)                     | real `CFRunLoop` thread serves commands        | behavior while AX sources are attached                                  |
| `_AXUIElementGetWindow`                                         | injected lookup, absent symbol path            | symbol presence on macOS 26                                             |
| Tabs (`AXTabGroup`/`AXTabs`)                                    | fake tab groups shared between windows         | Finder, Ghostty single-tab probed read-only; the rest unverified        |
| Cleanup, quarantine, retry                                      | scripted removal failures                      | real `AXObserverRemoveNotification` failure modes                       |

Tiling's read-only actor (slice 04: `state/`, `actor/`, `events/`, `tracking.rs`) is tested with no real window, display or process. `state/tests.rs` drives the pure `transition` function; `events/tests.rs` drives the bounded pipeline, geometry coalescer and mailbox with explicit `Instant`s; `runtime_tests.rs` runs the engine against `FakeWindowBackend` with a `ManualClock`, an inline effect runner and `FakeAppDirectory`, plus a few real-thread tests (a gated backend that holds `track` open, concurrent snapshot readers, a saturated effect queue, cancellation). The suite covers enumeration blocked while the event stream advances, destroy and same-app `CGWindowID` reuse, title/ignore/system/Picture-in-Picture exclusions, fullscreen/minimized/hidden/tab flags, 30/60/120/360/out-of-range/`NaN` refresh rates, a full semantic queue and its single reconciliation, sink overflow and sequence gaps, backend disconnect, mailbox and effect-queue saturation, a slow actor under a flood, cancellation between check and sleep, no-screen and hotplug retiming, two registries and late old completions, create-focus-destroy ordering, an unresolved termination during an in-flight application list causing exactly one follow-up list, geometry coalescing across identity barriers, atomic snapshots under concurrent reads and zero native effects (the fake backend never receives an action), and one test that drives the real owner `Core` over `FakeWindowAx` and fake observers through `TrackingRuntime` (initial track, a created and focused window, a refresh that keeps surviving tokens, a closed window destroyed). The caught-panic test is labelled **unwind-only**: the release profile aborts, so it is not release recovery evidence. The harness is internal and unregistered; real observer delivery, ordering and permission behavior stay **unverified**.

Tiling's animation pacing (animation part 03: `stache_macos::display_link`, `animation/{pacing,size}.rs`, `pacing_tests.rs`) is tested with a **fake frame clock** and never creates a real `CVDisplayLink`: `FakeFrameClockFactory` fires ticks by hand (`fire(display, at)`), fails creation per display or for all (`fail_creation`, `failing`), holds a callback in flight (`fire_held`, a barrier) and can make a stop stuck (`set_stuck`), with logs of attempts, creations, stops and live clocks; the real clock and the fake share one `ClockCore` (latest-wins slot, `stopped` flag, in-flight counter), so the stop-safety protocol is exercised by the fake. A crate's own default tests pace with a failing factory (the timer fallback); an end-to-end test injects `TrackingOptions::frame_clocks`. Covered: ticks per display at different rates with one batch per application, coalescing of several ticks into one frame, the stall fallback, idle and disabled owning no clock, a creation failure using the timer with a diagnostic that clears, a held callback delaying the stop, a stuck clock quarantined and retried, the pure size rule (interpolated size on every frame for growth, shrink, mixed axes and cross-display moves, a pure move writing no size before the final frame, an instant job, a refused final size), the same through the shard backend fake, and end to end over the real sharded adapter (one frame per tick, sizes written at the fake AX ramping monotonically to an exact target for growth, shrink and a cross-display move, three windows through grid, master and monocle on a 120 Hz display with slow writes ending exactly on target, a stop with a clock running). Real display-link behavior stays **unverified**.

Tiling's frame effects (slice 05-A: `state/frames.rs`, `outcome.rs`, `reservations.rs`, `actor/frames.rs`) are tested with fakes only. `state/frame_tests.rs` drives the pure transition: desired versus confirmed frames, A–B–A plan sequences with an old acknowledgement unable to settle a newer plan, only changed components sent, `Applied` only after a matching readback (admission, a missing readback or a mismatch is `Partial`/`Uncertain` with the actual frame recorded), proven-no-send as typed not-attempted/stale reasons, one attempt per plan with no retry on own echoes or refreshes and exactly one external-change attempt, user `Relayout` and config/topology changes as the only new attempts, bounded in-flight writes and inference, and the reservation fixtures (top native 24 / host 40 / own 6 gives 46 not 70, all four edges, native larger than host, negative origins at scale 1 and 2, infeasible and non-finite bounds, an empty snapshot as zero host inset, a display absent from a present host topology, tombstones, a reused native `DisplayId` against a stale host presence, stale fetch versus successor). `outcome.rs` tests the native-outcome classification. `reservations.rs` tests the SDK `EdgeInsets` conversion and a scripted `ReservationSource` through the watcher (read-then-wait, forwarding, cancel and close wake, mailbox close). `runtime_tests/frames.rs` runs the engine and `FrameControl` against `FakeWindowBackend` (which models the owner gate and can hold a send): a gated write settled from the readback, no retry after a refusal or echo, inert superseded and sealed writes, a finite cancellation-wakeable drain, and real-thread cases where `TrackingRuntime::seal` returns while a send is held, queued writes never reach the backend and shutdown seals before it waits. A fake barrier does not prove a real OS call can be preempted; real AX writes, the owner's native revision check and real reservation publication from the host registry stay **unverified**.

Tiling's lifecycle (slice 05-B: `module.rs`, `status.rs`, `module_tests.rs`, the `TilingFactory` tests in `stache-app` `host.rs`) is tested with fakes over the real `Registry` and `ConfigStore`: `FakeWindowBackend` (failpoints, `hold_sends`, scripted stop and `retry_cleanup` outcomes), a scripted reservation source, a gated application directory and a recording teardown trace. TI-05-01: inert construction, `Running` before the (stalled) enumeration completes, a failpoint at each of the six start stages with install/stop counts matching and a clean restart, a backend refusal, an incomplete unwind quarantined and retried, `WaitingForPermission` without any request, status projection. TI-05-03: the coordinator is held after it observes a revision and the runtime provably still runs the old one until the same revision is flipped, a rejected candidate leaves runtime, authority, epoch and the watch pair unchanged, A-B-A revisions, an invalid reload held and repaired independently of a retained cleanup error. TI-05-04: stop seals while a send is held, drain precedes the restore seam which precedes watcher/epoch/actor/worker, a timed-out drain quarantines truthfully with the restore stage not run, a restart is refused while quarantined, a retry runs only the owed stages, an unacknowledged stop is re-issued, a successor is unaffected by the late old completion, waits wake before joins. TI-05-05 (factory part): reservations are read and delivered before authorize, and a stopped or quarantined Tiling leaves an independent module running. Nothing in these tests tiles real windows; real AX and the real registry host shutdown path are **unverified**.

Tiling's virtual workspaces (part 06 slice A: `workspaces.rs`, `intent.rs`, `state/intents.rs`, `state/workspace_tests.rs`) are pure and tested through `transition` with no backend, thread or native call. TI-06-01: an empty configuration giving one default workspace per display, `main`/`primary`/`secondary`/name assignment, a missing display falling back to main and returning with its membership intact (while a user-relocated workspace is not pulled back by a later display event), one visible workspace per display with one focused workspace and focus history, no displays meaning no frame effects and an explicit incomplete topology, rule then own-display then display-default assignment, a title change never moving a user-managed window, non-Grid insert-after-focus versus Grid append, promotion and swap, focus/swap/send/layout/resize/balance intents and their exact token grammar (case-insensitive focus with `prev`, case-sensitive six-value swap), and the structural reload mapping (add, remove into the visible workspace in stable order, rename as remove plus add, user layout kept until the configured layout changes, default-layout edits reaching only followers, screen-assignment moves, rule and ignore edits changing eligibility without reassigning, and a rejected candidate leaving the old state). TI-06-02 (pure part): the same app in two workspaces or on two displays is never hide-wanted while a visible workspace needs it, the partial-isolation conflict is published, and repeated refreshes leave the requirement unchanged. Preset precedence (an explicit preset in a Floating workspace beats default centering, a silent admitted no-op elsewhere even for an individually floating window), centering once, inert `preset_on_open` and no Grid-overflow automation are covered. Part 06 slice B (`visibility.rs`, `visibility_tests.rs`, `runtime_tests/visibility.rs`, the `module_tests.rs` `visibility` module, the `windows/tests.rs` `AXHidden` cases) adds the runtime-owned visibility ledger, the executors and the restore at stop, tested with fakes only: a real `ApplicationControl` over `FakeApplicationBackend` (barriers before the commit and inside the send), `FakeWindowBackend` scripted `AXHidden` outcomes and the real registry. TI-06-02: the pure `Ledger` (a prehidden or externally hidden application never owned, an external show relinquishing and suppressing a re-hide until needed again, an application needed by two workspaces or displays never planned, `Requested` not a confirmation, an uncertain hide kept `PendingPossible`, a show while a hide may have landed kept `Ambiguous` and never a restore candidate, a terminal gone clearing, capped normal unhide retries, no hide and unhide of one application in one plan); the engine together with the ledger (requirement-driven hide, unhide and no oscillation, the published conflict, no hide after the seal, focus unhiding an owned-hidden application before `activate_exact` and then raising through the backend gate, a refused or stale activation typed and never raising); `AXHidden` as a fallback only when the public hide returned `false`, with a typed confirmed or uncertain result, owner-confined and exact-instance scoped. TI-06-03: a hide cancelled by the seal before its send never issues, a committed one is drained and restored, restore unhides only runtime-owned exact instances (never a replacement or a prehidden one), an unconfirmed or refused unhide stays owed with its reason, quarantines the stop (`restore` owed) and a retry restores only the remaining records, ambiguous records are reported through status and `ambiguous_visibility` and never unhidden, and the stop order stays sealed, drained, restore, watcher, epoch, actor, runner. Real `NSRunningApplication` hide, `AXHidden`, activation and `AXRaise` behavior stay **unverified**. Part 06 slice C (production registration) adds TI-06-04 with fakes only: `stache-app` `host.rs` tests build the production list plus `TilingFactory::production` over a main thread that runs nothing and prove Tiling is registered default disabled with nothing started; over fakes (`FakeWindowBackend`, `FakeDisplayBackend`) enabling reaches `Running`, the native display layout reaches the actor and the observer is removed on stop; missing Accessibility is `WaitingForPermission` with zero permission requests and nothing acquired; requested animation and borders publish unsupported diagnostics while the module stays `Running` and healthy; and a quarantined Tiling leaves a real noTunes module (over `FakeApplicationBackend`) `Running`. `module_tests.rs` `production_surface` adds the display-feed failpoint at every start stage (observer count returns to zero), the initial layout before `authorize`, a change delivered through the worker, the stop order (`Watcher`, `Displays`, `Epoch`), a failed observer removal owed and retried, a refused observer failing the start with nothing acquired, diagnostics set and cleared on reload, and a partial placement counted in the status. `visibility_tests.rs` covers `ActionsDirectory` over `FakeApplicationBackend` (exact instances of the run's generation, a failed query yielding nothing) and `LazySystemBackend` spawning no adapter or shard thread until used; `macos` covers `ApplicationObservation::running_details`. The real `SystemWindowBackend`, `SystemDisplayBackend` and `NSWorkspace` enumeration are never constructed by default tests; their live behavior stays **unverified** and needs per-run approval.

Tiling's socket methods and CLI (part 07: `dto.rs`, `ipc.rs`, `ipc_tests.rs`, `cli.rs`, `cli/render.rs`, `cli/render_tests.rs`, `cli_tests.rs`) are tested with fakes only. `ipc_tests.rs` drives `TilingIpc` against a fake actor that applies the real pure transitions (queries, the fixed action order, first and middle failure, deadlines, shutdown, pause and resume). `cli_tests.rs` (TI-07-01 to TI-07-03) runs the **composed** CLI (`stache_cli::run_with_io` with the tiling contribution) against a real temporary Unix socket, the real `Router`, the same handler and the real client serde and renderers: every query table and JSON golden (compact, detailed and apps, Unicode and escaped titles, every filter alone and combined, empty bodies and `[]`), ANSI output that strips to the plain golden, every window and workspace action alone and combined in fixed order, a numeric focus id and an unknown one, a late resize failure keeping earlier actions, a middle failure, queue saturation, an unconfirmed send (`uncertain`, sent once), disabled, not-initialized and shutting-down queries, a host with a different config directory, a connection timeout, an absent host (no socket is created and nothing starts) and the parse (exit 2) versus semantic (exit 1) split. A canned transport covers the fault adapters: every typed client error from both the status and request phases, every wire error kind, hostile control characters, partial and uncertain outcomes, a pause that is not `applied`, malformed or unknown-field data that never echoes the payload, and the exact wire JSON each flag produces. `render_tests.rs` pins table glyphs, padding, alignment, truncation, `(no title)`, the frozen JSON key order and the legacy error bodies. Goldens are derived from the pinned legacy `tabled` layout and `cli/output.rs`, never produced by running legacy code. `cli_binary.rs` runs the real binary for help, completions and the absent-host path. Real AX, window movement and live display behavior stay **unverified**.

Tiling's passive drag (part 08 slice B: `drag.rs`, `drag_tests.rs`, `state/drag.rs`, `state/drag_tests.rs`, `runtime_tests/drag.rs`, the `module_tests.rs` `drag_input` module) is tested with fakes only; no test installs a tap, sends a mouse event or moves a window. `drag_tests.rs` pins the pure geometry (49/50/51 point thresholds, Euclidean centre travel, two windows both moved beyond 50 so the greatest travel wins, inclusive edge containment, first match in stable order over overlapping originals, the dragged window never its own target, the 5 point resize rule, one ratio fixture per layout) and the bounded drag lane. `state/drag_tests.rs` drives the gesture through `transition`: capture of exact tokens and frames, suppression without spending an external attempt, swap, snap-back (including after the external budget was spent), stale tile list, layout, revision, destroyed window and registry restart at commit, resize ratios per layout, floating windows neither captured nor pulled back, own-write echo rebase versus a genuine user change, and every lost observation cancelling once with one refresh. `runtime_tests/drag.rs` runs the engine on the manual clock (timeout, reads off the callback, flood). `drag_input` runs the real registry over the SDK `FakeBackend` input service: one listen-only install and no suppressing tap, stop order and the tap closing with the last passive subscriber (another passive subscriber keeps it), three suppressing fakes at descending priority consuming a press that Tiling still sees without changing any decision, a disabled listen-only tap and an epoch change cancelling the gesture, a refused install or an unattached service degrading drag only. Real taps, Input Monitoring prompts and real mouse drags stay **unverified**.

Tiling's opt-in animation (part 08 slice C and the asynchronous pipeline, `animation/`, `state/animation_tests.rs`, the `module_tests.rs` `animation` module, `runtime_tests/animation.rs`) is tested with fakes only; no test moves a window. `animation/tests.rs` pins the legacy easing, spring and transition formulas, the 200 ms default and the distance scaling below 500 px (50/117/200 ms fixtures, 100–1000 ms clamp). `animation/coordinator_tests.rs` runs the coordinator over a fake clock and a fake sink that records every batch, answers frames inline or holds their replies for the test to answer (written, replaced, refused, partial, stale), and follows a fake window position for the readbacks: the first/intermediate/final stages and the single settle read, one batch per application per tick, the retarget read, its failure and its timeout falling back to the last acknowledged frame, A, B, A acknowledging each plan once and bringing the window back, a superseded or sealed job with a frame in flight completing only when that frame answers (reporting it when issued), replaced frames counted as dropped, a window destroyed mid-animation ending stale, a seal or abort writing nothing, and the worker thread (prompt wake on cancellation, staying alive after close until the held frame answers, a latest-plan bounded mailbox, an inert live-reconfigured control). `animation/pipeline_tests.rs` runs the same coordinator through the real `LiveSink` and `FrameControl` over `FakeWindowBackend`, whose per-application shard model (`hold_application`, `script_frame_for`, `script_dispatch_error_for`, `frame_batches`, `sent_frames`, `replaced_frames`) keeps one pending frame per window with the real replace and supersede rules: a held application not delaying another and its replaced frames dropped (TA-02-01), no readback on intermediate frames and exactly one at the final frame, one extra read at a retarget (TA-02-02), a partial or refused frame ending the job, and a seal during a held write deferring the completion and the drain until it answers (TA-02-03). `runtime_tests/frames.rs` adds asynchronous direct writes (the effect runner returning while a write is held) and the full-coordinator fallback to one direct write. `runtime_tests/animation.rs` wires the real actor and tracking runtime to the real sharded `WindowAdapter` over `FakeWindowSystem::factory` (a spy backend records batches and snapshot reads; `FakeWindowAx::on_node_write` stalls or slows one application's writes) with several applications and windows: a layout change in per-application batches reaching exact targets, an animation interrupted and sent back, a slow application skipping frames while the others stay on time, a workspace switch with direct writes only, a drag cancelling an animation, and a stop mid-animation (seal, drain waiting for the held write, then the coordinator join and the restore, with `frame drain` owed while the write is stuck). `state/animation_tests.rs` covers the pure plan (off by default, `from`/refresh rate, an animated window in motion never assumed arrived, bounded echo suppression, a drag gesture cancelling and blocking). The `module_tests.rs` `animation` module runs the real registry and a real coordinator over the fake window backend: default off owns no coordinator, reload enable/disable, gated frames acknowledging the exact plan, stop mid-animation (sealed, drag, drained, animation, restore, ...), and a held send quarantining with `frame drain` and `animation worker` owed, restore not run, and a retry draining before the restore. The Enhanced UI lease (part 04) is tested with fakes only. `FakeWindowAx` models `AXEnhancedUserInterface` per application node (`FakeNode::enhanced_ui` and `set_enhanced_ui_result` for read, set and restore failures, `Call::EnhancedUi`/`Call::SetEnhancedUi` in the ordered call log, `enhanced_ui_writes()`, and `on_enhanced_ui_set` to simulate a third party turning it back on mid-lease) and `windows::testing::FakeVoiceOver` is the settable VoiceOver probe. `windows/tests/lease.rs` drives the owner `Core` with the stepping clock (a lease open before the first write and closed after the 50 ms grace around animated and direct writes, chained batches toggling once, an application already off or lacking the attribute never written, a re-enable detected once, counted, not fought and reaching `consecutive: 2`, read and set failures proceeding without a lease, a failed restore quarantined then restored exactly once by `retry_cleanup`, stop restoring without waiting for the grace, an application terminated mid-lease dropping the lease with no write, a shard panic restoring) and the real threaded adapter (the parked shard waking at the grace deadline, `restore_enhanced_ui` running only after a held write drained, an owed restore reported, VoiceOver meaning no attribute call at all). `module_tests.rs` checks the stop order (lease restore after the frame drain and the animation join, before the runtime-hidden restore; a failure leaves `enhanced ui restore` owed) and `status.rs` the diagnostic lines. `runtime_tests/animation.rs` runs it over the real sharded adapter: one application with the attribute on toggles off once and back on once around a whole animation while one already off is never written, a stop mid-animation restores after the drain, and a third-party re-enable shows in the status. Real AX writes, frame pacing and smoothness stay **unverified**.

No test or default check contacts JankyBorders: the external-service path was removed when Stache began drawing borders itself (Tiling borders part 08).

Tiling's whole-desktop scenarios (`runtime_tests/scenarios.rs`) wire the real actor, transition, visibility ledger, frame control and application actions over fakes and drive them like a desktop: the user's workspace rules, nine applications (an agent, a 1x1 utility window, a Finder info popup, one that cannot be tracked), one built-in display, the system's hide/unhide notifications delivered promptly, twice, late or never, Cmd-Tab, intents and the frontmost application at startup. They pin that one failed application never blocks the layout of the others, that workspace switches hide and unhide correctly over repeated round trips, that focus follows the activation's real focused window and never switches a workspace on a guess, that an application activated while its hide is unsettled does not ping-pong the workspace, that a lost hidden notice resolves on the actor clock, that an application missed at listing is tracked after its activation or the watch install, and that agents and tiny windows are not tracked. `cli_tests.rs` reads a response far over one 4 KiB chunk through the real socket (macOS rejects a socket timeout update after the host closed its end), and `stache-sdk` pins the client against a large answer followed by an immediate close. None of these touch a real window or application.

Tiling's animation metrics and instant fallback (`animation/metrics.rs`, `animation/fallback.rs`, `animation/fallback_tests.rs`, `state/animation_tests.rs`, `status.rs` tests, `runtime_tests/animation.rs`) are tested with fakes only. The coordinator tests drive `Animator` over the fake clock and `FakeSink` (`manual` holds replies, `set_overflow` hosts an application on the overflow shard; `FakeWindowBackend::set_overflow` does the same for the live sink) and answer a "slow application" late or as replaced: the recorded frames, rate, write latency, end lag (measured against the producing tick or timer deadline, with ticks at the job period at every phase never causing a fallback at 60 or 120 Hz while a latency above one period still does) and the fixed 64-sample ring; a stalled clock giving timer-rate frames and timer attribution; a contained panic completing an ending job exactly once; the bounded book (the last 8 animations, 64 applications, least recently used); one bad animation not triggering and 2 of the last 3 triggering for the rate, the lag, the overflow and the no-frame-clock reasons; a full coordinator not being sticky; one exact final write and no further frame when an application enters the set mid-animation (also while waiting for its retarget read); no write after a settle readback; a reload that turns animations off clearing the set and the history; a terminated or relaunched application; and other applications unaffected. The planner tests pin one direct write per plan for an application in the set, A, B, A included. The status tests pin the exact `animation: <name> (<bundle id>) moves without animation for <kinds>: <reason>` and `animation metrics: ...` lines. The end-to-end tests run a desktop of several applications over the real sharded adapter where one application's fake AX writes are slow: it crosses into fallback mid-run, its next layout change is one direct write per window, the others keep animating, status shows the diagnostics and metrics, and a relaunch animates again; a third party that keeps re-enabling Enhanced UI reaches the same fallback. The fallback set tests also pin the cool-down (60 s, doubling to 10 minutes, reset by a healthy record, sticky Enhanced UI reasons), the entry and exit notes for the log, the per-kind policy (moves and resizes counted and cut short separately) and the cleared history on leaving; `state/workspace_tests.rs` pins that preset A, B, C, A (with a retarget mid-animation and a position-only change) animates for a healthy application. `runtime_tests/borders.rs` counts border moves against the frames a slow fake application really wrote for one and two windows, a retarget and a direct write. The SkyLight batching part was skipped, so nothing tests it.

Manual and native gates (each needs per-run approval; the formal per-run native gates in `validation.md` have not
run): enumerate and observe one real application, write a frame to a scratch window, raise it with
`activate_exact`, revoke Accessibility permission while tracking, quit a tracked application, and the
animation measurement (the fixed four-application scenario in [performance](performance.md#animation-measurement-method): effective rate, latency, end lag, Enhanced UI, fallback, threads and CPU on a named display). The animation measurement is **not run**: it has no binary, runs only when the user approves that exact run, and every number it would produce is unverified. An
informal real-desktop run on 2026-11-09 confirmed workspace switching, CLI focus, queries and the
master layout; it is not a recorded gate.

The Caps Lock remap (`stache-macos` `caps_remap`, `stache-keybindings` `caps`, the host recovery
step) never runs the real `hidutil` and never touches the real mapping, event taps or Application
Support. One `World` fake holds the global mapping, the journal file and the lease and numbers
every transition (each read, each journal create/fsync/rename/directory-sync/unlink, each native
write), so a test crashes the process before or after any one and checks that recovery restores the
exact starting mapping, that the owned mapping never exists without a trusted journal, and that
an unverified restore never deletes it. Other tests drive the real journal and lease over temporary
`HOME` roots (symlinked journal or directory, replaced lock inode, loose modes, a busy lease
expiring on a manual `LeaseClock`, separate identities), the strict `UserKeyMapping` parser over
a table of malformed shapes, and `/usr/bin/hidutil` through the shared executor with a fake
process factory and clock (exactly 5 s, exactly 1 MiB / 64 KiB, 2 s cleanup, 64-slot reaper
saturation, unkillable children). Module tests run the chord state first as a pure table and then
through the real input service router and press ledger with `FakeBackend` (every key and
alias, bare tap, repeats, key-up after Caps release, tap loss, reload mid-press), and the owner
over a fake tap: the remap is applied only after an acknowledged tap, stop invalidates dispatch
before any restore work, and a failed restore keeps an inert safety-only subscription. The host
suite runs the recovery step over the fake world (stale journal with the module disabled or its
config invalid, no journal, foreign mapping, failure reported once without a retry loop). Real
remap, restart and crash-recovery checks stay **unverified**; each run needs approval, exact
process and journal identification, saved before/after mapping evidence and verified restoration.
Socket-binding tests (`ipc`, the app `host` integration suites, the config watcher) cannot run in
a sandbox that forbids local binds; run them unsandboxed.

`cargo doc -p stache-sdk -p stache-macos -p stache-keep-awake -p stache-keybindings -p stache-hold-to-quit -p stache-menu-anywhere -p stache-app -p stache-proxy-audio -p stache-notunes -p stache-cli --no-deps`
builds their public docs; pass `RUSTDOCFLAGS='-D warnings'` in validation.
The shared command runner is tested over an injected process factory, child handles and
a fake clock (poll quantum, 2 s cleanup transfer, 64-slot exhaustion, captured deadline
and caps); `app/tests/internal_cli.rs` feeds the runner's rewritten argv through
`dispatch` with a desktop selector that panics, for standalone and
`.app/Contents/MacOS/stache` identities. Only a few harmless `/bin/sh` fixtures spawn
real processes; no test runs a configured command.
Socket tests use temp directories and temp sockets only; no test reads or writes
the real cache, runtime, configuration or legacy identity.

## Automated checks

CI rules (`.github/workflows/`):

- Every `uses:` pins a full 40-character commit SHA followed by a `# vX.Y.Z` comment naming that
  release. Dependabot (`.github/dependabot.yml`) checks GitHub Actions weekly and opens one grouped
  pull request that moves the SHAs and comments together.
- The token is read-only by default: a top-level `permissions:` block grants only `contents: read`,
  and a job that needs more declares it on that job alone (`quality.yml` has none).
  `actions/checkout` sets `persist-credentials: false`.
- Lock guard: in each job, `cargo fetch --locked` runs before the first step that runs cargo, the
  Tauri CLI, `bunx`, `npx` or a `bun <script>` other than `install`, `audit` or `pm`, failing when
  `Cargo.lock` does not match the manifests; the job ends with
  `git diff --exit-code -- Cargo.lock bun.lock`. Local scripts are not locked.
- `taiki-e/install-action` installs the exact cargo-sort, cargo-nextest and cargo-audit versions
  listed above; every `tool:` entry carries `@<version>`. Dependabot moves the action SHAs, but
  these `tool:` versions are bumped by hand.
- The pin test also rejects local actions, quoted `uses:` values and flow-style steps, by design.
- The Quality job sets `NEXTEST_PROFILE=ci` (`.config/nextest.toml`), because the runner is slow
  and starved: no fail-fast, so one run reports every failing native test; a failed test is
  retried up to twice and nextest lists one that then passed as `FLAKY`; the restore-settle tests
  each get the whole machine. The profile leaves out every test of the two Tiling animation test
  modules (`runtime_tests::animation` and `module_tests::animation`, 29 tests): nearly all run the
  real engine on real time, and the runner is slow enough that the engine falls back to direct writes
  (`BelowMinimumRate`) even when a test runs alone. They are a local check: `bun run test` and
  the pre-push hook use the default profile, which runs them and never retries.

- `lint:ui`: strict typecheck of frontend, prepare script and root configs, then oxlint (including `@stylexjs/eslint-plugin` and the local `scripts/lint/stylex-animation.js` through `jsPlugins`, and the
  `src/ui/src/ipc/` import boundary; see [UI](../architecture/ui.md#conventions)), then stylelint over `src/ui/**/*.css`.
  oxlint enforces `curly: all`: every `if`, `else`, `for` and `while` body uses braces, even for one
  statement. A blank line separates every `if`, loop, `switch`, `try` and bare block from the
  statements before and after it, a blank line precedes every `return`, and one follows each run of
  `const`/`let`/`var` declarations (`@stylistic/padding-line-between-statements`). The start and
  end of a block need none.
  `lint:native`: cargo-sort check and Clippy with warnings as errors. Neither fixes source.
- `format:check`: pinned oxfmt and rustfmt checks, including active specs. `format` explicitly rewrites.
  Root oxfmt disables nested configs so legacy config cannot override archive exclusions. Generated
  artifacts (`schemas/**`, `src/crates/app/gen/**`, `src/ui/src/ipc/bindings.gen.ts`) are excluded:
  their bytes belong to the generators, so `check:generated`, not the formatter, is their gate.
- `test` runs `test:ui`, `test:e2e`, `test:scripts` and `test:native`. Bun runs commands; Vitest and
  Playwright Test run TS tests.
- `test:ui`: Vitest browser mode in real Playwright WebKit. Suites are colocated `*.test.ts(x)` under
  `src/ui/src`; `src/ui/tests` holds setup and helpers (`render`, `recordIpc`). Coverage retains
  80/80/65/80 thresholds. `tests/setup.ts` pins `prefers-reduced-motion: no-preference` before
  each file renders and after every test, so the machine's Reduce Motion setting never applies;
  a test that wants reduced motion emulates `reduce` itself (`playwright.config.ts` pins the same
  default for `test:e2e`). Keyboard-focus tests do not rely on Tab, which WebKit lets reach buttons
  only when macOS "Keyboard navigation" is on: they focus the element and press a real key, which
  `:focus-visible` treats as keyboard focus.
- **Screenshot baselines** (`src/ui/e2e/screenshots/*.spec.ts`, part of `test:e2e`):
  Playwright `expect(locator).toHaveScreenshot(name)` in WebKit with animations
  disabled, the caret hidden, a fixed viewport and device scale factor 2, and
  `page.emulateMedia({ colorScheme })` for dark and light. `snapshotPathTemplate` in
  `playwright.config.ts` stores baselines in
  `src/ui/e2e/screenshots/__screenshots__/<spec file>/<name>.png`. Current baselines:
  `BarItem` rest, hover and active in dark and light (`bar-item.spec.ts`, harness page
  `/e2e/harness.html?gallery=bar-item`), and the end zone with the five system widgets
  in dark and light (`system-widgets.spec.ts`, production `index.html?surface=bar` with
  the fake host and `Date` fixed by `page.clock.setFixedTime`), and the Weather item
  ready and loading in dark and light (`weather.spec.ts`, the provider answered by a
  `page.route`, every other remote request aborted), and the Media item playing with
  artwork, paused and without artwork in dark and light (`media.spec.ts`, the artwork a
  PNG fixture from `tests/png.ts`), and the Spaces strip wide (every app) and compact
  (the focused app only) in dark and light (`spaces.spec.ts`, reduced motion so the
  strip is settled), and the Accessibility guide blocked and revoked (480×640, the
  page captured with a transparent background) in dark and light (`windows.spec.ts`, reduced motion so nothing is mid-animation),
  and the tray popover shell with every badge kind, its Overview with the user's modules and
  the settings editor on `keepAwake`, `tiling` and an invalid `tiling.toml` (480×640,
  transparent background) in dark and light (`popover.spec.ts`). The popover and the
  Accessibility guide use the system font and WebKit's system colours; their specs pin the
  user-dependent ones (accent, selection, focus ring) to the macOS defaults and paint a
  neutral stand-in for the native material (`pinSystemColours`, which both specs import
  from `e2e/screenshots/systemColours.ts`), so
  they need no font skip. The
  other baselines depend on the
  system font 'Maple Mono NF': after loading, each test calls `skipWithoutMapleMono(page)`
  (`e2e/screenshots/font.ts`, a DOM measurement evaluated in the page, since WebKit's
  canvas does not see user-installed fonts) and, without the font, calls
  `test.skip(true, "font 'Maple Mono NF' is not installed: screenshot baseline skipped")`,
  so the run reports an explicit skip and never a silent pass. A missing baseline fails
  the test and writes the new reference for review. After an intended visual change,
  regenerate and review them with `bunx playwright test <path> --update-snapshots`
  (for example `bunx playwright test src/ui/e2e/screenshots/bar-item.spec.ts --update-snapshots`).
  Screenshot tests never live next to source.
- `test:e2e`: Playwright Test in WebKit against the Vite dev server on port 1430 with the scripted fake
  host (`src/ui/e2e/`); scenarios are `src/ui/e2e/scenarios/*.spec.ts`, screenshot specs `src/ui/e2e/screenshots/*.spec.ts`, and output goes to the ignored
  `test-results/e2e/`. See [UI](../architecture/ui.md#test-harnesses).
- `test:scripts`: the Vitest Node project in `vitest.scripts.config.ts` covers the root TypeScript
  tooling under `scripts/`. The teardown collision guard uses injected fake queries only; no test
  reads the real process table, signals or launches a process. `scripts/measure/config.test.ts` pins
  the TOML that `writeNextConfigs` writes (`global.toml`, `proxyAudio.toml`) and that
  `writeLegacyConfig` keeps writing JSONC for the legacy app. `scripts/ci/workflows.test.ts` reads
  every `.github/workflows/*.yml` and `*.yaml` as text and enforces the CI rules above, naming file
  and line. `scripts/media-control/vendor.test.ts` runs the `vendor:media-control -- --check` logic
  on the vendored media adapter and its manifest, and covers `--verify-upstream` with an injected
  fetch and a synthetic bottle built in the test; it never touches the network. The real
  `bun run vendor:media-control -- --verify-upstream` (ghcr.io and raw GitHub) runs in CI only.
  `scripts/release.test.ts` drives the release pipeline with an injected fake command runner
  over temporary directories: it spawns no build and touches no real bundle.
- `test:native`: cargo-nextest across only the root workspace with
  `--features stache-app/dev-tools`; checks desktop identity/window policy, the SDK lifecycle, config
  and window suites, app host races/permissions/shutdown, the proxyAudio policy and fake-backend
  lifecycle, the noTunes blocker sweep/launch/reconfigure/stop policy, the Keep Awake fake
  lock/assertion/observation matrix and its host status/socket suites, the macOS audio
  listener-rollback tests and the macOS adapter unit tests. Native tests
  never prompt for Accessibility and never change the system default audio device; the real
  main-thread/window smoke test is the opt-in `stache-smoke` harness described below.
  Generator binaries require `dev-tools` and are excluded from the release bundle.
- `cargo nextest run -p stache-sdk -p stache-cli -p stache-proxy-audio -p stache-notunes -p stache-keep-awake -p stache-keybindings -p stache-macos`
  runs the config/audio/blocker/keep-awake foundation. The proxyAudio lifecycle suite uses a fake backend for
  every listener, worker-spawn, set-default and stop-race failpoint; no test starts a real listener.
  The noTunes suite uses `FakeApplicationBackend` and `ManualExecutor` for every observation, sweep,
  launch, open and stop failpoint; no test observes, terminates or launches a real application.
  The Keep Awake suite uses fake assertion, lock-probe, observation and clock inputs; no test
  acquires real power prevention or interacts with the real screen lock.
  `cargo doc -p stache-sdk -p stache-macos -p stache-keep-awake -p stache-keybindings -p stache-proxy-audio -p stache-notunes -p stache-cli --no-deps`
  builds their public docs.
- `generate-schema` writes the committed `schemas/*.schema.json` from the registered types.
  `generate-schema -- --check` writes nothing and fails on missing or stale files; config templates are
  generated from `Default` and schemars descriptions, never from a hand-written copy.
  TOML coverage: `sdk` unit tests for the loader (line/column spans, date/time and `nan`/`inf`
  rejection, whitespace-only versus comment-only files), the `toml_edit` tray edit and the template
  emitter; `app/tests/config_catalog.rs` for template order, the untagged-union and keybinding-key
  round trips and the `i64` bound; and `app/tests/toml_scenarios.rs`, the end-to-end scenarios that
  run `config init`, edit the files, reload through the store, toggle `enabled` as the tray does,
  check `core.status`, and migrate the full legacy fixture to `.toml` files that load with zero
  warnings.
  `generate-bindings -- --check` exports to a secure temporary file and compares bytes; it never
  writes the committed bindings or source.

Oxfmt supports Markdown; verify its pinned version with a disposable malformed Markdown file.
Oxlint is JS/TS linting, **not Markdown prose linting**. No semantic/prose replacement is claimed.

## Hooks and scope

Prepare installs standard Husky in a normal checkout, without sidecar setup. CI sets `HUSKY=0`.
Linked-worktree prepare explicitly **skips installation** to preserve shared Git config; it does not
fabricate shims. Existing shared hook configuration is inherited. If `.husky/_` shims are absent,
run `PATH="$PWD/node_modules/.bin:$PATH" sh .husky/pre-commit` and `sh .husky/pre-push` explicitly
from that worktree. No automatic shared configuration change is made.

Pre-commit checks active staged inputs; Rust stages run workspace format/sort/Clippy checks.
`lint-staged.config.ts` uses one function task because lint-staged globs cannot combine the
extension sets with the nested archive/build/generated exclusions, and `tsc -b` takes no filenames. Staged `.css` files also run stylelint.
It passes literal argv to the installed tools, so names with spaces or apostrophes stay safe. Pre-push runs `bun run test`.
Check routing manually in a disposable checkout for TS, TSX, Markdown, Rust and ignored-only
stages.
Vite uses its default watcher rooted at `src/ui`; native crates, archive and root build outputs are
outside that root. No custom watcher module or ancestor-matching ignore is needed.
Smoke HMR once with `bun run dev`: edit/restore the `UnknownSurface` text (open `/` with no surface) and observe an update without navigation;
write an ignored archive/nested-worktree probe and observe no update. Stop the owned server with Ctrl+C.

Correction smoke (2026-11-02): Shell edit/restoration hot-updated with the same page time origin and
window marker; archive/nested-worktree probes stayed quiet for five seconds. The owned server/browser
were stopped. Disposable hook checks passed Markdown, TS with spaces/apostrophes, TSX styles and Rust
workspace routing; malformed Markdown/TS and invalid CSS failed. Legacy/worktree-only stages ran no
tools. Normal prepare installed Husky in the disposable repo; linked prepare preserved shared config.

## Native dev-watch acceptance (manual)

This check is outstanding after the tooling redesign; do not infer it from a bundle build.
Use two terminals in the worktree. In terminal A run `bun run tauri:dev` and wait for the Next tray
and CLI watch messages. Keep this foreground session so Ctrl+C stops the dev session you own.
In terminal B, preserve the current uncommitted source and trigger a harmless recompile:

```sh
backup=$(mktemp "$PWD/target/native-watch-source.XXXXXX")
cp src/crates/app/src/lib.rs "$backup"
printf '\n// Native watch acceptance probe.\n' >> src/crates/app/src/lib.rs
# Observe recompilation and restart in terminal A, then restore:
cp "$backup" src/crates/app/src/lib.rs
# Observe the restoration recompile/restart, then leave source untouched.
legacy_probe=$(mktemp -d "$PWD/_legacy/.native-watch.XXXXXX")
nested_probe=$(mktemp -d "$PWD/.worktrees/native-watch.XXXXXX")
build_probe=$(mktemp "$PWD/target/native-watch.XXXXXX")
touch "$legacy_probe/probe.rs" "$nested_probe/probe.rs" "$build_probe"
# Observe no rebuild/restart for at least five seconds after these ignored writes.
# Stop terminal A with Ctrl+C before cleanup in terminal B:
rm -- "$legacy_probe/probe.rs" "$nested_probe/probe.rs" "$build_probe" "$backup"
rmdir -- "$legacy_probe" "$nested_probe"
lsof -nP -iTCP:1420 -sTCP:LISTEN
```

Confirm source restoration before removing its saved copy. Check lsof reports no owned dev server
and the Next process launched by terminal A exited. Never use killall/process-name termination or launch legacy.
Record the actual restart/quiet-period/cleanup observations before marking this acceptance complete.

## Native host smoke (opt-in)

The harness's WebKit probes read the popover Overview's DOM: `[data-testid=overview]`, the
`proxyAudio` row (`[data-module=proxyAudio]` with `data-state="disabled"` and an unchecked
`role=switch`) and, after the reload, the `[data-file="proxyAudio.toml"]` configuration note
carrying the reload's `smoke_marker` warning. `OverviewTab.test.tsx` pins those attributes.

`cargo nextest run --workspace --features stache-app/dev-tools` covers the host
composition with fake native and permission services: early quit, repeated
shutdown, blocked-start/shutdown races, queued main-thread drain,
watcher/enable/invalid/conflict cases, permission polling, ticker rescheduling and
window leases. The Accessibility launch gate is covered with a fake `PermissionProvider` whose
status a test changes between ticks, a request counter, a recording `SettingsOpener`, recording
fake modules and a fake startup-command runner: blocked launch, grant without restart, revocation
in reverse order, `Unknown` as `Denied`, request/opener policy and shutdown while blocked (host
fixtures opt in with `HostServices.baseline`; the default is empty). No test calls
`AXIsProcessTrusted*`, opens System Settings or launches the app. The teardown coordinator is exercised with an injected clock, fake
terminal and recording record channel: a blocked configuration commit ends
`Uncertain` naming `host-worker` and `module:<id>` and still dispatches, a later
request shortens but never extends the total, repeat and post-outcome requests
change nothing, a blocked record sink is `unavailable` without delaying the
terminal action, two concurrent requests give one run and one outcome, and only a
`Clean` restart relaunches after releasing the instance claim. Bounded stages use
the same injected time over a real registry: a hung stop, blocked start and hung
module worker are each named `module:<id>` and abandoned while the next module
stops, modules unreached at the total are named in reverse start order, and a hung
watcher, permission ticker, startup job or accept thread is detached with the
owned endpoint still removed. Owner fakes acknowledge, fail or hang, so a missing
answer is unacknowledged and can never produce `Clean`; a recording executor
proves a main-thread owner is answered only by its own closure, while one stalled
hop names every main-thread owner and the tray hop stays separate from exit. A
window release answers only after `window_closed` for every label, including one
released by a stopping module, and an already-gone window counts as closed. The
socket suite checks a failed owned-inode removal, a foreign inode as success and a
hung handler as unacknowledged. The forced exit is a fake that records its code and
instant. It never launches Tauri, a tray, a webview, a real system prompt,
`terminate:` or a relaunch.

H10 runs the real run loop through a separate, feature-gated binary. Use the
provided per-user temporary root, not a fixed `/tmp` directory, and check the
socket path before launching:

```sh
bun run build   # embeds the production React bundle
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  cargo run -q --release -p stache-app --features smoke --bin stache-smoke
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

This template produced a 103-byte resolved socket in the approved opencode
temporary root. Do not add another `s` or extend the `XXXX`; a 105+ byte socket is
the deliberate failure probe below, not the normal run.

`smoke` implies `custom-protocol`, so the harness loads the embedded production
assets rather than a dev server. It proves zero webviews, `run_on_main` on the
AppKit main thread, the activation observer delivering on main and removing itself
on drop, the popover opened through the host (`NativeWindows::open_popover`; the
prewarm is compiled out under `smoke`) with its React Overview rendering the disabled
`proxyAudio` row, generated `host_status` IPC inside WebKit, a pre-start
`proxyAudio.toml` loaded as a valid disabled config, a real socket reload
accepted from a changed valid config that stays disabled (with one ignored
`smoke_marker` key), the resulting `statusChanged` update advancing the exact
revision in the `host_status` snapshot and showing the new warning in the React DOM,
the popover hidden (webview and bookkeeping kept, never destroyed), the host still
answering on its socket and not shutting down while it is hidden, the popover shown
again as the same document, the second process
rejected by the instance lock, and a real SIGTERM delivered only to the owned
PID. It exits 0 after the shared shutdown path completes. The reload never
enables `proxyAudio`, so smoke never starts the audio runtime or changes the
default device. It never reads real configuration, launches legacy automation or
prompts for permissions.

The teardown terminate observation proof (`TEARDOWN-01-01`, `TEARDOWN-01-02`) is a
separate scenario of the same binary, selected only by `STACHE_SMOKE_TERMINATE`.
It **requires per-run approval**: do not run it as part of the checks above, and
never run the `stache-smoke` binary built below without that approval.

```sh
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  STACHE_SMOKE_TERMINATE=1 \
  cargo run -q --release -p stache-app --features smoke --bin stache-smoke
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

Without `STACHE_SMOKE_TERMINATE` the binary runs the checks described above
unchanged. With it, the scenario uses the shipped observation and coordinator —
it installs neither — calls the real `terminate:` on its own process only, and
prints this evidence while `AppKit` waits for the coordinator's single reply:

```text
smoke: ok: applicationShouldTerminate: reached the shipped teardown coordinator
smoke: ok: a main-queue job ran while the terminate reply was pending
smoke: ok: a tao proxy event ran while the terminate reply was pending
smoke: ok: the coordinated teardown owns the reply; waiting for termination
```

Each step is bounded to 5 s and the process has 10 s after the last probe to
terminate; a missing step prints `smoke: FAIL: …` and exits non-zero. The
coordinator prints its one `teardown: outcome=…` record before the reply. The
scenario never signals or terminates another process, never terminates by
executable name and does not enable a module. Logout, restart and shutdown remain
unverified and need the disposable-session procedure in the spec.

The opt-in real-process failure probe deliberately makes the socket too long and
must exit non-zero even though `run_return` reports zero on macOS:

```sh
BASE=$(mktemp -d "${TMPDIR%/}/opencode/s.XXXX")
LONG="$BASE/$(printf '%080d' 0)"
mkdir -p "$LONG"
HOME="$LONG" TMPDIR="$LONG" XDG_CONFIG_HOME="$LONG/config" \
  ./target/release/stache-smoke
test $? -eq 1
rm -r -- "$BASE"
```

It prints `host setup failed: socket path … is longer than 103 bytes` and
`desktop host exited with code 1`, and returns **1**. The same assertion runs as
`cargo nextest run -p stache-app --features smoke --test smoke_failure`.

Observed 2026-11-03 in this worktree: every `smoke: ok:` line printed and the
normal process exited 0; the deliberate long-socket process exited 1. A second
manual run is required on any machine that changes the Tauri/window stack; do not
infer H10 from the bundle build alone. Tray-click and SIGINT delivery remain
manual because automating a menu-bar click is outside the owned process.

## Teardown verification (Part 04)

Read before validating the coordinated teardown. Parts 01–03 are committed. No
test or scenario here quits, logs out, terminates, signals or relaunches a real
process by executable name.

### Real-thread host scenarios

`src/crates/app/tests/host.rs` runs the assembled coordinator with the real
system clock, real threads and fake owners registered through
`Teardown::register_owner`. A real thread drains `ManualExecutor`, so a
main-hop owner is answered only by the closure that thread runs. Each owner
scenario yields exactly one outcome and one outcome record attempt, and leaves
the coordinator and host runner finished; the blocked-sink scenario additionally
attempts its forced-exit terminal evidence after the outcome slot is readable:

| Result                                         | Test                                                                          |
| ---------------------------------------------- | ----------------------------------------------------------------------------- |
| `Clean`                                        | `real_threads_all_owners_acknowledge_clean`                                   |
| `Partial`                                      | `real_threads_a_failing_owner_is_partial`                                     |
| `Uncertain` at the owner bound                 | `real_threads_a_hung_owner_is_uncertain_by_the_deadline`                      |
| blocked `host-worker` despite acknowledgements | `real_threads_a_blocked_host_worker_is_uncertain_despite_acknowledged_owners` |
| escalation ends a hang                         | `real_threads_escalation_ends_a_hung_owner_within_the_escalation_bound`       |
| concurrent requests                            | `real_threads_concurrent_requests_produce_one_run_and_one_outcome`            |
| blocked record sink                            | `a_blocked_record_sink_keeps_the_slot_readable_and_bounds_the_forced_exit`    |

Timing assertions are bounds, not exact instants; the escalation bound carries a
scheduling allowance for parallel suite runs. The blocked-sink scenario waits for
the normal total deadline before the forced exit, so it is the one host test that
costs about five seconds. The deliberately hung owner in the `Uncertain`
scenarios is named in the record instead of being silently left behind.

### Opt-in harness scenarios

Each scenario is selected explicitly for one run and needs approval for that run.
The binary remains behind the `smoke` feature and is built from this worktree.
Use a per-user temporary root and check the socket path first:

```sh
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  cargo run -q --release -p stache-app --features smoke --bin stache-smoke
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

`STACHE_SMOKE_TERMINATE=hang` keeps the terminate observation scenario and also
registers a temporary hanging HUD owner, `smoke-hang`, that is never answered.
Its `release` prints exactly:

```text
smoke: ok: hanging owner entered
```

The owner's bound is the normal 2 s owner wait, so a parent has that window to
send a second request. Expected evidence is one `teardown: outcome=uncertain`
record whose `unacknowledged` list names `smoke-hang`, one `AppKit` reply with
its terminal record, and the exit status from `AppKit`. Any other non-empty
`STACHE_SMOKE_TERMINATE` value keeps the previous behaviour.

`STACHE_SMOKE_HOLD=1` starts the normal app with that hanging owner registered,
schedules no `terminate:`, shows the popover the smoke checks use,
and waits for an external quit. It prints:

```text
smoke: ok: popover stache-popover visible
smoke: ok: holding for an external quit
```

Bring Stache frontmost (the popover is key) and press Cmd-Q (twice for the repeat case). With
nothing arriving it prints `smoke: FAIL: no external quit arrived within 120 s`
and exits non-zero. `STACHE_SMOKE_HOLD=clean` is the same without the hanging
owner. A successful run ends inside `AppKit`, so the verdict is the stderr log
plus `wait "$PID"`.

`STACHE_SMOKE_RELAUNCH=1` sends the first process through the shipped
`request_restart` path after readiness. Before requesting, it writes a marker
under its own `AppPaths` runtime root; a process that finds the marker is the
successor. The successor must hold the instance lock, reach `ready == true`, and
print:

```text
smoke: ok: first process ready; requesting a coordinated restart
smoke: ok: relaunched successor reached readiness
```

The successor then requests an ordinary shutdown and exits by itself. Every wait
is bounded with `smoke: FAIL`. The first process is replaced by the restart, so
`run_return` does not return: the verdict is the parent's reading of the log, not
the process result.

These scenarios never touch a module's real resources; the smoke binary still
writes the disabled `proxyAudio` baseline before the host starts. Run
`bun run build` first so the embedded production assets exist, then build the
harness binary:

```sh
bun run build
cargo build --release -p stache-app --features smoke --bin stache-smoke
```

### Manual gates

One gate at a time, approved for that individual run. Never unattended, never
bundled with another gate. Target only `com.marcosmoura.stache` (these are release builds).
Granting Accessibility to a test `Stache.app` built in `target/` will likely invalidate the grant of
an installed `/Applications/Stache.app`, and the reverse: macOS keeps one Accessibility entry per
bundle identifier, tied to the code signature, and ad-hoc signatures differ per build (not yet
observed). The
env-selected scenarios live in the feature-gated `stache-smoke` binary; the
`bun run tauri:build` bundle is the shipped binary and is used where a gate calls
for the bundle itself. Both run with a temp-root environment and every module
disabled (`stache-smoke` writes the disabled `proxyAudio` baseline; the bundle
reads the isolated `HOME`/`XDG_CONFIG_HOME`):

```sh
ROOT=$(mktemp -d "${TMPDIR%/}/opencode/g.XXX")
SOCKET="$ROOT/com.marcosmoura.stache/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
BUNDLE="$PWD/target/release/bundle/macos/Stache.app"
HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
  "$BUNDLE/Contents/MacOS/stache" 2>"$ROOT/stderr.log" &
PID=$!
```

The harness scenarios use the same environment with the binary instead:

```sh
HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
  ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
PID=$!
```

Temporary roots isolate files and instance locks, not LaunchServices addressing
or the login session. Before any identifier-addressed event, record the
collision query and require it to pass:

```sh
bun scripts/check-next-instance.ts "$PID"
```

It prints the `query:` snapshot and the `result:` verdict, exits non-zero on a
foreign instance, an unknown identity or a failed query, and never sends
anything. A running development build (`com.marcosmoura.stache.next`) is a distinct
identity: it is not a collision and it is not treated as the owned instance.
Recheck immediately before the event and prevent concurrent launches for the
run.

Record for every gate: the command, the outcome and terminal evidence (including
record delivery availability, late reply and extended bound), socket absence, and
the `wait "$PID"` OS status. Keep `recorded_code` separate: `exit(7)` records 7
in the teardown outcome, while an unchanged `dispatch` may still return OS status
1; the AppKit status is a fact, not the outcome. The owned socket is absent when
`test ! -S "$ROOT/com.marcosmoura.stache/ipc"` holds.

- `TEARDOWN-04-02` — owned terminate with a temporary hanging owner:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_TERMINATE=hang ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  wait "$PID"; echo "status=$?"
  grep 'teardown:' "$ROOT/stderr.log"
  test ! -S "$ROOT/com.marcosmoura.stache/ipc"
  ```

  Pass: one `Uncertain` record naming `smoke-hang`, one reply with its terminal
  record, no owned socket, and the AppKit exit status recorded.

- `TEARDOWN-04-03` — hold without a hanging owner:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_HOLD=clean ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  ```

  After `smoke: ok: popover stache-popover visible`, with the popover key, press Cmd-Q
  once, then `wait "$PID"`. Pass: one coordinated run, one `Clean` record, no
  owned socket. If the key never reaches AppKit, the gate fails and requires
  explicit re-planning with the owner; app-menu or quit-event evidence is
  supplementary only. Turn off legacy `commandQuit` first: while legacy Stache
  runs with it enabled, its event tap swallows Cmd-Q before this process sees it.

- `TEARDOWN-04-04` — hold with the hanging owner:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_HOLD=1 ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  ```

  First run: bring the window frontmost and press Cmd-Q twice. Second, separate
  run: start `STACHE_SMOKE_TERMINATE=hang` instead, wait for
  `smoke: ok: hanging owner entered`, then send SIGTERM to `$PID` from the parent,
  so the signal is the second request. Pass: within 250 ms of the second request the outcome has
  `escalated=true` and `Uncertain`, with one reply. If AppKit does not deliver
  the second quit while a reply is pending, record that; the SIGTERM result must
  still pass.

- `TEARDOWN-04-05` — the harness's own `NSApp` `terminate:`:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_TERMINATE=1 ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  wait "$PID"; echo "status=$?"
  ```

  Pass: one coordinated `Clean`, one reply, no owned socket. Optional
  `osascript -e 'tell application id "com.marcosmoura.stache" to quit'` is
  allowed only after the collision guard passes; it proves only that
  supplementary external route.

- `TEARDOWN-04-06` — logout with the app running, only in a disposable login
  session or on a spare machine approved for that run, with evidence collected
  outside the affected session. Pass: one record within budget, no owned socket,
  the logout proceeds, and the built `Info.plist` does not enable sudden or
  automatic termination. Without a disposable session or spare machine, record
  **not run**; Part 04 stays blocked. The same isolation is mandatory for any
  approved restart or shutdown evidence.
- `TEARDOWN-04-07` — SIGTERM to `$PID` from the bundle with no prior request.
  Pass: one `Clean` record, no owned socket, and `wait "$PID"` status 0.
- `TEARDOWN-04-08` — the coordinated relaunch:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_RELAUNCH=1 ./target/release/stache-smoke 2>"$ROOT/relaunch.log" &
  PID=$!
  for _ in $(seq 1 60); do
    grep -q 'relaunched successor reached readiness' "$ROOT/relaunch.log" && break
    sleep 1
  done
  wait "$PID"; echo "status=$?"
  grep 'smoke: ok:' "$ROOT/relaunch.log"
  ```

  Pass: the first line, then the successor line, with the successor holding the
  instance lock and reaching readiness instead of stopping at the early
  `AlreadyRunning` return; it then exits on its own. The first process's result
  and the log are the evidence because the restart does not return through
  `run_return`. This exercises the direct-spawn path; the bundled `open -n` path
  remains blocking for the Reload item's follow-up.

- `TEARDOWN-04-09` — the fake-query tests above prove refusal. Each approved
  identifier-addressed run records the `bun scripts/check-next-instance.ts "$PID"`
  output before the event. Never launch or signal a real conflicting instance to
  test the guard.

This procedure claims no gate as passed; each run's evidence lives in the
[part's completion notes](../specs/2026-11-03-teardown/04-verification-gates.md#gate-evidence). The orchestrator records the
per-run evidence. A pass covers that run only, and a change to terminate
observation, budgets or stage order requires a rerun.

## Non-activating panel live gate (opt-in)

The automated suite proves the panel kind, placement, reposition and lifecycle
phases and host call order with fakes only; the real AppKit panel hosting, level
and Space behavior were verified by the 2026-11-09 run recorded below; a change to the hosting needs a rerun. It needs per-run approval and is never part
of `cargo nextest`, `lint` or CI. The `panel-native-check` feature implies
`custom-protocol` and cannot be combined with `smoke`. It runs the real Tauri
app against an isolated root (the same disabled `proxyAudio` baseline as smoke),
opens the existing `test` surface as a non-activating panel at a frame relative
to the top-left of `NSScreen.screens[0]` (the menu-bar display, not `mainScreen`)
and closes it after `--hold-seconds` (default 60, max 600). Production
validation still rejects a panel for `test`; only the feature's
`NativeWindows::open_test_panel` bypasses that check.

Native evidence (macOS 27.0, user-approved probes, 2026-11-09; macOS 27 evidence counts for the macOS 26+ gate by user decision 2026-11-10, macOS 26 itself not separately verified):

| Design                                                                                                                         | Result                                                                                                                                                                                                                                                                 |
| ------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `object_setClass` of Tauri's window to an `NSPanel` subclass + `NonactivatingPanel` bit                                        | Click still activated Stache; close crashed in 3 of 4 runs (`NSRangeException`, cannot remove KVO observer `_windowLayerContext` from the subclass, also `opaque`). The window is already `NSKVONotifying_TaoWindow`; restoring the class does not help. **Rejected.** |
| Plain Tauri window + `NonactivatingPanel` bit set after creation                                                               | Click activated Stache. The bit has no effect after creation. **Rejected.**                                                                                                                                                                                            |
| Native `NSPanel` created with `Borderless \| NonactivatingPanel` at init, Tauri's webview moved into it, Tauri's window hidden | Click did not activate Stache; close was clean. **Works; this is the shipped design.**                                                                                                                                                                                 |

The harness keeps Tauri's window hidden and logs
`panel-check: activation: frontmost=<app> stache_active=<bool>` (read-only, 100 ms
poll, on change only). The test page cannot report hover, so hover stays a manual
checklist item. Use a height of 300: 40 points only shows the test page header.

```sh
bun run build
cargo build --release -p stache-app --features panel-native-check --bin panel-native-check
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  ./target/release/panel-native-check --x 0 --y 0 --width 900 --height 300 \
  --hold-seconds 90 --i-understand-this-opens-a-real-panel
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

Before the run, focus another application (for example TextEdit with a typed
cursor). The harness prints `panel-check: phase Requested`, `Created`, `Loaded`,
then the checklist hint; after the hold it prints `phase Closing`, `Closed` and
`ok: Tauri closed the panel and its webview`. Record each observation:

1. Clicking controls in the panel (the test page buttons) does not activate Stache,
   keep the Dock/menu bar on the previous application, or move keyboard focus away
   from the frontmost application's cursor. The log must show `stache_active=false`
   and an unchanged `frontmost=` after every click.
2. The panel sits below normal windows: an ordinary window dragged over it covers
   it; it is not above the menu bar and not above floating panels.
3. Hover (mouse moved/entered over the page) updates while another app is active
   (manual; the page cannot report it).
4. After the hold: `Closing`/`Closed` phases appear, the panel disappears, the
   process does not crash on close (no `NSRangeException`), no stray process or
   window remains, and the app exits 0.
5. Anything that fails must be recorded with the exact case. A private SkyLight
   adapter is not preauthorized; ask for explicit permission before proposing one.

Native Spaces and fullscreen Spaces are optional, low-priority observations:
Stache does not build on native Spaces. The panel keeps legacy's
`canJoinAllSpaces | stationary` behavior (no `fullScreenAuxiliary`) for parity.
The 2026-11-09 run passed items 1–4 and both optional observations.

## Bar system providers (part 05, slice a)

All provider checks are fake-backed and never read the machine: the `stache-macos` `system::testing` fakes
(scripted CPU usage, CPU temperature, battery, `CoreWLAN` interfaces; a script can be held to simulate a hung native read),
a scripted SMC key table behind the `SmcOpener`/`SmcConnection` traits (`system/smc/tests.rs`: every decoded type, invalid, out-of-range and
non-finite values, per-chip key selection, the bounded retry and last-valid rules, open failure and no keys giving unknown),
a fake `ProcessFactory` for the `ipconfig` summary (on by default: exactly one bounded child per sample; `STACHE_BAR_WIFI_SSID_SUMMARY=0` spawns none; stalled, oversized and malformed children) and a virtual
`Clock`, so schedules are asserted exactly (`providers/tests.rs`): CPU 0.25 s then every 2 s, battery 30 s
charging and 120 s otherwise, Wi-Fi 5 s, a slow sample never overlapping and a 10-minute stall giving one late
sample, a thousand Wi-Fi signals giving one extra sample at least 500 ms later. The runtime tests
(`modules/bar/src/tests.rs`, `provider_runtime`) run real worker threads over the fakes: zero reads when
disabled, zero-size, no-primary or paused, readings flowing through the published cell with unchanged samples
emitting nothing, a hung CPU read not delaying battery/Wi-Fi/bridge reads, bounded stop with a retained owner,
no republish after stop, resume restarting the workers. The end-to-end scenarios are in
`src/crates/app/src/bar_tests.rs`. The WebKit side was deleted with the previous UI on `feat/ui-rebuild`. SMC temperature was checked in the approved live run (it matched Activity Monitor); the performance run is a
manual gate that needs separate approval (see [features/bar](../features/bar.md) and [performance](performance.md#bar-system-providers-part-05)).

## Streaming and media adapter (part 07, slice A)

`commands/stream/tests.rs` covers the line assembler as a pure function (UTF-8 split at every byte, multi-line chunks, a line exactly at the cap and one byte over, an oversized or never-terminated line, invalid UTF-8, an unterminated tail, buffer never above the cap) and `LineStream` over fake children with a fake clock (on-demand reads, stderr flood bounded, cancel while blocked, `SIGTERM` then reap, `SIGKILL` after the grace, reap timeout retained, drop kills). The few real-process cases run only `/bin/sh -c` one-liners with no descendants. An idle stream waits on `poll(2)` over the child pipes plus a cancel wake fd with no timeout, so a silent child causes zero wakeups. `media_resources.rs` tests run over temp directories (missing, wrong type, not executable, relative input, architecture gate, packaged-versus-development selection). **No default test spawns the real adapter, perl or any media process**; running the vendored adapter is a separately approved manual gate. The bundle check is manual and compile-only: `bun run tauri build --debug --bundles app --config src/crates/app/tauri.conf.json`, then inspect `target/debug/bundle/macos/*.app/Contents/Resources/media-control` and run `codesign --verify --deep --strict` on the app.

## Bundles and prerequisites

The Cargo lock pins `tauri =2.12.5`, the same minor as the pinned
`@tauri-apps/api 2.12.4`; the Tauri CLI rejects a Rust/JS minor mismatch
("Found version mismatched Tauri packages…"), so the two move together. The rest
of the lock only carries what that pin requires: `tauri-build =2.7.4`,
`tauri-runtime`, `tauri-runtime-wry`, `tauri-utils`, `tauri-codegen`/`tauri-macros`,
`wry`, `tao`, `muda` and `tray-icon`. Cargo audit currently reports upstream
maintenance/unsoundness warnings. The Bun lock pins patched Vite/Vitest and
tested happy-dom/minimatch/tinypool overrides.
`bun run tauri:build` passes `--features custom-protocol`; a release build without it fails early
because Tauri would otherwise ship a dev-mode window. Generate/schema binaries are feature-gated and
not bundled.

Launch only `target/release/bundle/macos/Stache.app`, verify tray/Quit and zero windows, then quit.
The bundle needs Accessibility for Stache before any module runs (the launch gate; grant it for that bundle's entry, not a development build's `Stache Next` entry); it needs no audio permission. Local bundles use ad-hoc signing; `bun run release --self-sign`
signs with the owner's self-signed certificate, and nothing is notarized. CI audits new locks, not the archive. Legacy build
verification is opt-in via [_legacy/README.md](../../_legacy/README.md), including resource/signing gaps.
Never run legacy and new device-changing automation together.

## Release artifacts

`bun run release` builds the release bundle, verifies it (contents, `arm64`, system-only
linkage, the vendored `media-control` set, merged `Info.plist`), packages an ad-hoc DMG and zip,
mounts and expands them to check the app and its signature, and writes `SHA256SUMS` and
`release.json` to `target/release-artifacts/`. `bun run release --self-sign` signs inside out with
the certificate named by `STACHE_SIGNING_IDENTITY` and verifies authority, hardened runtime and a
certificate-anchored designated requirement (`RELEASE-03-11` to `03-15`). `scripts/release.test.ts`
covers both modes, the refusals, the fail-closed paths and the removal of the paid path with fake
runners; nothing touches a keychain, signs with a real identity or contacts Apple.
See [release](release.md). The same file checks that no cask remains (`RELEASE-05-11`: no cask
module, exactly the DMG, zip, `SHA256SUMS` and `release.json`, two-entry `SHA256SUMS`) and the
[install guide](install.md)'s doc check (`RELEASE-05-06`: sections, the not-notarized statement and
Open Anyway route, no Homebrew route, links, no process-name, quarantine-bypass or `brew install`
commands, `--force` only on `stache config migrate`).
`scripts/release/publish.test.ts` drives `release:publish` with a fake runner (`RELEASE-06-10` to
`06-12`: each refusal, the single draft `gh release create`, the notes) and checks the published
limitations in `README.md` and the install guide and the checklist order in the release guide
(`06-13`, `06-14`); it contacts no GitHub, creates no tag and publishes nothing.

Legacy-config note (`RELEASE-05-03`/`04`): `cli/tests/commands.rs` runs `config path` over
temporary roots (present, absent, `global.toml` present, `<id>.jsonc` also present) and checks
the directory stays byte-identical; `app/tests/legacy_config_hint.rs` drives the host's startup
`log_legacy_config_hint` with an unreadable and a directory-shaped source.

Symlink CLI probe (`RELEASE-05-07`, probe half): `cli/tests/symlink_probe.rs` copies its own test
executable to `<tmp>/Probe.app/Contents/MacOS/stache`, links it from `<tmp>/bin/stache`, and runs
it directly, through the link and by name on `PATH`. In child mode the copy only prints
`current_exe` and `stache_cli::mode::is_app_bundle`'s verdict, so the real `stache` binary is
never executed. Expected: `true` directly, `false` with the link path through the link.

## Release performance comparison

`bun scripts/measure-foundation.ts` (also `bun run measure -- --help`) measures
release startup, 60-second idle CPU/RSS and process trees with isolated config
roots. It refuses to start while any Stache process (or the explicit `--legacy-app`) is
running and never signals another instance. Build the bundles and follow
[performance.md](performance.md) before running it; do not run a manual
audio-device test at the same time.

Identity (release part 04): `app/tests/identity.rs` checks that the base `tauri.conf.json` holds the release identity and `PRODUCT_NAME`, that `tauri.dev.conf.json` holds exactly the development `productName` and `identifier` and only `tauri:dev` passes it (after the base config), that `tauri:build` has one `--config`, `bundle.resources` only `media-control` and no release profile enables debug assertions, that `IDENTITY` is selected only by `cfg!(debug_assertions)`, that the development literals stay in the allow-list, and that no tracked script or source uses `killall`, `pkill`, `pgrep` or compares a process name to `stache` (a text check with its limits stated in the test). Tests compile as debug, so they run under the development identity; `AppPaths::resolve_for` lets `sdk/src/paths.rs` and `cli/src/cache.rs` cover both identities' exact paths, the socket limit and that `cache clear` under one leaves the other intact. `app/tests/collision.rs` drives the startup `collision::guard` over `FakeRunningInstances`: a foreign same-identifier process refuses with the diagnostic and nothing after the check runs, a sibling and a failed query warn and continue, own PID and prefix matches are ignored, and the fake records only the two queries; no test queries real applications.
