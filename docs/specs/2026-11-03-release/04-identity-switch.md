---
status: in-progress
---

# 04 — Identity switch

Read when preparing or performing the staged rename to `Stache` / `com.marcosmoura.stache`.
**PLANNED AND GATED:** the rename cannot run until the legacy app is removed and approved for
removal. Preparation depends on [Part 01](01-build-metadata.md); the rename also depends on
[Part 05](05-install-upgrade-uninstall.md), which documents the removal.

## Scope

Rename every release identity surface in one commit, keep development on `.next` permanently,
decide what happens to `.next` directories, and detect collisions without ever signaling
anything. This part cannot be `done`, and
`feat/release` cannot merge, until the rename has landed. See the
[overview](00-overview.md#blocked-gates-and-merge-consequence).

## Stages

| Stage        | What happens                                                               | Gate                          |
| ------------ | -------------------------------------------------------------------------- | ----------------------------- |
| 0. Today     | Everything is `.next`. No change.                                          | None                          |
| 1. Prepare   | Derived display name, inventory test, warn-only collision detection.       | Part approval                 |
| 2. Authorize | The owner records migration approval and that legacy is removed (Part 05). | Recorded owner approval       |
| 3. Rename    | One commit changes every surface and adds the dev overlay. Refusal begins. | Stage 2 recorded in this file |
| 4. Rebase    | In-flight rewrite branches pick up the overlay when they rebase.           | None                          |

## Identity surfaces

The inventory, refreshed 2026-11-13. It names areas, not every file: Stage 1 derives the
exact list with a search for both literals, and its test keeps that list complete.

| Surface                 | Locations                                                                                                                                                                                                  |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cache and runtime roots | `IDENTITY` and its doc comment in `src/crates/sdk/src/paths.rs`; the isolation claim in `sdk/src/instance.rs`                                                                                              |
| Bundle                  | `productName` and `identifier` in `src/crates/app/tauri.conf.json`; `description` in `app/Cargo.toml`                                                                                                      |
| Generated app info      | `name` in `app/src/bindings.rs::app_info`; the identifier already reads `IDENTITY`                                                                                                                         |
| Tray and window strings | `TOOLTIP` and the Quit label in `app/src/tray.rs`; title in `app/src/windows.rs`; `app/src/access.rs`, `app/src/media_resources.rs`, `app/src/catalog.rs`                                                  |
| CLI                     | `about` in `cli/src/commands.rs`; `cli/src/lib.rs`. `is_app_bundle` in `cli/src/mode.rs` matches `.app/Contents/MacOS` and needs no logic change; only its test path changes                               |
| Tests asserting `.next` | `sdk/src/paths.rs` tests, `cli/src/cache.rs`, `cli/tests/commands.rs`, `cli/src/mode.rs`, `app/tests/cli_binary.rs`, `app/tests/bindings.rs`, `app/src/lib.rs`                                             |
| UI                      | `src/ui/index.html`, `e2e/harness.html`, the Accessibility guide (`windows/AccessibilityGuide/`), the Popover Overview footer, their tests and e2e scenarios                                               |
| Other crates            | Caps remap ownership strings in `macos/src/caps_remap/` and `modules/keybindings/src/caps/`; `sdk/src/durable.rs`; `modules/wallpapers/src/cli/tests.rs`                                                   |
| Tooling                 | `name` and `tauri:dev` in `package.json`; `scripts/measure/` `paths.ts`, `config.ts`, `launch.ts`, `process.ts`, `run.ts`, `report.ts`, `cli.ts`; `scripts/measure-bar/`; `scripts/check-next-instance.ts` |
| Docs                    | `README.md`, `AGENTS.md`, `docs/index.md`, `docs/architecture/`, `docs/guides/testing.md`, `docs/guides/performance.md` and the `docs/features/` pages that name the app                                   |
| Not changed             | `_legacy/`, archived specs, and `~/.config/stache`, whose path never depended on the bundle identity. Development keeps `.next` through the overlay                                                        |

## Requirements

### Stage 1: preparation

- **Display name.** `app_info`, the tray and window titles read the name at runtime from
  `AppHandle::package_info().name`, so a `.next` dev run does not present itself as the release
  app. That value is compiled from the merged `productName`, and the bundler writes the same
  value to `CFBundleName`, which `RELEASE-01-06` proves on a real bundle. No `NSBundle` call is
  needed. `tray::rows` takes the name as an argument. `app_info` takes the `AppHandle`, which
  must leave the generated bindings unchanged.
- **Fallback.** A `PRODUCT_NAME` constant in the app crate covers only places with no Tauri
  context, such as the CLI `about` text. UI surfaces that show the product name read it from
  `app_info`; where copy must render before or without that answer (`index.html`, static guide
  copy), one UI constant holds the fallback.
- **Persistent ownership strings are not display names.** Where a `.next` literal marks state
  Stache owns across runs (the Caps remap owner, durable records), Stage 1 only inventories it;
  whether it follows `IDENTITY` is decided in the Stage 3 commit, so no owned state is orphaned.
- Add `src/crates/app/tests/identity.rs`. It asserts that `tauri.conf.json`'s identifier equals
  the release identity and its `productName` equals the constant, and fails when the other
  identity's literal appears in a tracked source outside a short allow-list. The allow-list holds
  the identity constants, the overlay, the collision code and tests.
- **Collision detection.** Add a narrow, safe query in `stache-macos` that returns the running
  instances for an exact bundle identifier, each with its PID and `NSRunningApplication.bundleURL`.
  `ApplicationObservation` exposes neither a URL nor a query without an installed observation, so
  this is new. It has no terminate capability.
- The desktop path checks after claiming the instance lock and before creating the tray, modules
  or socket. A CLI invocation never checks.
- In stage 1, every match only warns, under the [collision rule](#collision-rule). A running
  `com.marcosmoura.stache` is coexistence, which Decision 7 allows. `guides/testing.md` already warns
  against running legacy and new device-changing automation together.

### Stage 3: the rename

- Change every surface in one commit. The release identity becomes `com.marcosmoura.stache`; the
  base `productName` and `PRODUCT_NAME` become `Stache`. CLI `about` drops "Next". Generated
  output stays unchanged or is regenerated, and `check:generated` passes.
- **Replace, do not just rename, the assertions that encode separation:**
  `identity_paths_are_isolated_from_the_legacy_application` asserts the path does not contain
  `com.marcosmoura.stache/`, which the rename makes false for release builds by design. Its
  replacement asserts the exact paths for both identities and that `cache clear` under one cannot
  reach the other. `host_policy.rs`'s `config["identifier"] == IDENTITY` assertion splits the same
  way, since tests compile with the development identity.
- **Docs change meaning, not only names.** `ipc.md` says clearing can never reach the legacy cache,
  and the same claim is in `instance.rs`. After the rename a release build's cache path **is** the
  legacy path; a development build's is not. State the new truth. Rewrite the `AGENTS.md` "keep legacy/new identities distinct" hard rule,
  retaining "never terminate a process by shared executable name".
- **`scripts/measure`.** `NEXT_BUNDLE_IDENTIFIER` and `LEGACY_BUNDLE_IDENTIFIER` become equal, and
  the `/Stache\.app/` fallback in `process.ts` would classify the new app as legacy. Remove that
  fallback, classify legacy only from an explicit `--legacy-app` path, and keep the rule that the
  tool never signals another instance. Update `guides/performance.md`.

### Development identity

Added in the Stage 3 commit. From then on the release build uses the final identity and
development stays on `.next`.

- **Overlay.** Commit `src/crates/app/tauri.dev.conf.json`, holding only `productName` `Stache Next`
  and `identifier` `com.marcosmoura.stache.next`. The `tauri:dev` script adds it as a second
  `--config` after the base config. `tauri:build` keeps the base config alone.
- **Dev-only.** The overlay is never packaged: not in `bundle.resources`, no reference from
  `tauri:build` or `scripts/release.ts`. It carries no `version`, so the version check is unaffected.
- **Runtime identity (Decision 15).** The overlay reaches Tauri's config but not
  `stache_sdk::paths::IDENTITY`, which the lock, socket and cache use. Replace the single value with
  named release and development constants, and let `IDENTITY` select by `cfg!(debug_assertions)`:
  `.next` in debug builds, final in release builds. `tauri:dev` and `cargo run` are debug, so a CLI
  from a checkout reaches the dev host. `tauri:build` is release, and `[profile.release]` leaves debug
  assertions off, so a package cannot select `.next` unless that setting changes, which a test refuses.
- **Compile time only.** No environment variable, config key or CLI flag selects the identity. `HOME`
  and `TMPDIR` move the roots beneath it and never change it. `RELEASE-04-08`'s parameterized resolver
  stays a test seam: every production caller passes `IDENTITY`.
- **Local `--release` builds.** One is the release identity by design. While the released app runs, its
  desktop host exits as a second launch through the instance lock, before tray, modules or socket. That
  is the correct result, so add no other guard for it. A debug build, `tauri:dev` and a checkout's CLI
  all use `.next`.
- **Measurement.** `scripts/measure/launch.ts` already sets an isolated `HOME`, `TMPDIR` and
  `XDG_CONFIG_HOME` for every measured run, so a release-profile measurement shares no lock, socket,
  cache or config with an installed app, whichever identity the build selects. It needs no further
  isolation. The collision query is system-wide and ignores those roots, and the measure tool's
  exclusivity guard already rejects a running app by bundle identifier before launching.
- **Naming warts.** A desktop host started by plain `cargo run` skips the overlay, so it shows `Stache`
  while using `.next` paths; `tauri:dev` is the supported route. CLI `about` always reads `Stache`.
  `tauri build --debug` would pair the final bundle identifier with `.next` paths; the release script
  never passes it.

### Collision rule

**This process's identifier** is `IDENTITY`, which works for unbundled runs too. **The sibling** is the
other of the two constants. Compare identifiers exactly, and use the bundle URL only for the diagnostic.

| Running application, excluding this PID                   | Stage 1 | From Stage 3 |
| --------------------------------------------------------- | ------- | ------------ |
| Exactly this process's identifier                         | Warn    | **Refuse**   |
| Exactly the sibling identifier                            | Warn    | Warn         |
| Anything else, including a prefix match such as `.helper` | Ignore  | Ignore       |

- A release and a development run are normal siblings with separate locks, sockets and caches. So is
  a legacy app beside a development run. A legacy app beside a release shares the identifier and is
  refused.
- Because the instance lock is already held, another PID with this identifier cannot be a cooperating
  instance even at the same URL, which covers a legacy process whose bundle was replaced on disk.
- Unbundled development binaries have no bundle identifier and so are not detected. They are covered
  by the instance lock when they share this identity.
- **Refuse:** write one diagnostic naming identifier, path and PID, plus the Quit instruction, to
  stderr and the log; exit non-zero before tray, modules or socket. **Warn:** log one line naming the
  same three facts and continue. **Never signal, terminate or open anything.**
- A failed or unavailable query logs a warning and continues. It is advisory protection, and the
  instance lock remains authoritative for the shared identity.

### Directories

- `~/Library/Caches/com.marcosmoura.stache.next` and `$TMPDIR/com.marcosmoura.stache.next` belong to
  development from Stage 3 on. A release build never reads, migrates or deletes them. Removal is a
  documented user step in Part 05.
- A release CLI reports "not running" for a dev host, and a checkout's CLI does the same for a
  running release, since each resolves its own lock and socket.
- `~/Library/Caches/com.marcosmoura.stache` may hold legacy files, including `stache.sock`,
  `wallpapers` and `media_artwork`. The new app never reads them, and its socket stays under
  `$TMPDIR`. Part 05's removal step clears them; `stache cache clear` would also remove that tree.
- `config migrate`, `ConfigPaths` and the `~/.config/stache` location do not change.

## Acceptance criteria

- [ ] `RELEASE-04-01` The base `tauri.conf.json`, the release identity constant and `PRODUCT_NAME`
      agree, and the other identity's literal outside the allow-list fails. Test: `app/tests/identity.rs`.
- [ ] `RELEASE-04-02` `app_info`, tray and window strings use the name from `package_info`, the listed
      fallbacks use `PRODUCT_NAME`, and `check:generated` passes. Test: `app/tests/bindings.rs`, tray
      tests with an injected name, and the Accessibility guide and Popover footer tests.
- [ ] `RELEASE-04-03` Stage 1: a fake running instance with the sibling or the same identifier produces
      one warning, startup continues and nothing is signaled. Test: app test with the `stache-macos`
      fake backend.
- [ ] `RELEASE-04-04` Stage 3 rule: a foreign process with exactly this process's identifier refuses
      before tray, modules or socket, with the stated diagnostic and a non-zero exit; this process's own
      PID is excluded; and no signal path exists. Test: same harness, plus the absence of a terminate
      call on the fake.
- [ ] `RELEASE-04-05` Exact-identifier matching: the sibling identifier warns and continues under both
      build identities, a prefix match such as `com.marcosmoura.stache.helper` is ignored, and a query
      failure warns and continues. Test: same harness.
- [ ] `RELEASE-04-06` No code path identifies or terminates a process by the name `stache`, and no
      `killall` or `pkill` appears in `scripts/`, `package.json` or `.github/`. Test: `app/tests/identity.rs`.
- [ ] `RELEASE-04-07` Before Stage 3, this file records the owner's migration and removal approval
      with its date, and an `mdfind` for `com.marcosmoura.stache` on the release machine shows no
      remaining legacy bundle. Gated manual check.
- [ ] `RELEASE-04-08` After Stage 3, resolved cache, runtime and socket paths are exact for both
      identities, the socket fits the 103-byte limit under each, and `cache clear` under one leaves the
      other's directory intact. Test: `sdk/src/paths.rs`, `cli/src/cache.rs`, with the path resolver
      parameterized so a debug test run covers the release identity.
- [ ] `RELEASE-04-09` After Stage 3, a bundle at any `*.app/Contents/MacOS/stache` path still selects
      desktop. Test: `cli/src/mode.rs`, `app/src/lib.rs`.
- [ ] `RELEASE-04-10` After Stage 3, the measure tool never classifies the new app as legacy. Test:
      `scripts/measure/process.test.ts`, under `test:scripts`.
- [ ] `RELEASE-04-11` With a detectable `.next` bundle active, launching the release bundle warns
      and both run. With an unbundled `tauri:dev` run active, both run with isolated lock, socket
      and cache paths; no sibling warning is required because that process is not detectable.
      With a harmless placeholder bundle using `com.marcosmoura.stache` running, the
      release bundle refuses and leaves the placeholder running; quitting it and relaunching succeeds.
      Gated manual check, using temporary config roots and never the real legacy app.
- [ ] `RELEASE-04-12` After Stage 3, the base identifier is the literal `com.marcosmoura.stache`; the
      overlay holds exactly the two stated keys and values; only `tauri:dev` references it, after the
      base config; `tauri:build` has one `--config`; `bundle.resources` holds only the
      `media-control` entry and never the overlay; and neither
      `[profile.release]` nor a per-package release override enables debug assertions. Test:
      `app/tests/identity.rs`.
- [ ] `RELEASE-04-13` After Stage 3, with the release installed and running, `bun run tauri:dev` starts
      its own host instead of exiting as a second launch, shows `Stache Next` in its tray, and a checkout's
      `cargo run -q -p stache-app --bin stache -- cache path` prints the `.next` path. Gated manual check.
- [ ] `RELEASE-04-14` On a packaged build, the verifier's `CFBundleIdentifier` is
      `com.marcosmoura.stache` (`RELEASE-01-06`), and `stache cache path` through the shipped CLI route
      prints the final path. Gated manual check, recorded with `RELEASE-05-07` or `RELEASE-05-10`.
- [ ] `RELEASE-04-15` After Stage 3, `IDENTITY` is a constant selected only by `cfg!(debug_assertions)`,
      `AppEnvironment` carries no identity, and no `env!`, `option_env!`, config key or CLI flag reaches
      the selection. A debug test run sees the development constant. Test: `sdk/src/paths.rs` and a
      source check in `app/tests/identity.rs`.

## Tests

Collision tests use `FakeApplicationBackend`, extended as needed, and never query or signal real
applications. The full suite plus `check:generated` must pass in the Stage 3 commit.

## Notes

- A refused launch from Finder has no visible UI, like the existing silent second launch. A dialog
  would be a new surface, so this is published as a limitation in Part 06.
- macOS privacy records follow the identifier and signature. Legacy records for
  `com.marcosmoura.stache` were made under the self-signed `Stache App`, so they will not match the
  Developer ID build. Accessibility is an app-wide launch requirement (Accessibility launch gate), so the renamed bundle
  needs a fresh grant and shows the guidance window until it has one; Part 05 covers cleanup.
- Building `_legacy/` after the rename produces a bundle with the same identifier. `_legacy/` stays
  unchanged; Part 05 documents the warning.
