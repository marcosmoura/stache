---
status: draft
---

# 02 — GPUI prototype

Read when implementing after part 01 is done, after separate approval, only when the owner explicitly selects this part and every other roadmap workstream is finished or has no actionable part ([priority](00-overview.md#priority)). Each launch of the spike needs its own explicit owner approval.

## Scope

A standalone Cargo project at `spikes/gpui/` (decision 1) with its own `Cargo.toml`, `Cargo.lock` and `README.md`, outside the root workspace, plus the root config changes that keep root checks from seeing it (decision 2). Nothing in `src/`, the root `Cargo.toml` members, `stache-app` or the Tauri pins changes.

## Requirements

### Dependency choice

- Pin GPUI to an exact git rev of `zed-industries/zed` or an exact version of one republish (`gpui-unofficial`, `gpui-ce`, `xgpui`). The README records the choice, why, the license, the upstream rev it tracks, the last publish date and whether it builds with the repository's nightly toolchain (or needs a `spikes/gpui/rust-toolchain.toml`).
- Record build facts in the README with the exact command and its output: clean build time, dependency count, release binary size, and any system requirement (Xcode, Metal toolchain).

### Identity and isolation from Stache

- The spike bundle uses identifier `com.marcosmoura.stache.spike.gpui`; the coexistence binary uses `com.marcosmoura.stache.spike.coexist`. Product names do not contain "Stache" (for example `GPUI Spike`, `GPUI Coexist Spike`). A bare Cargo binary has no bundle identifier: the measured spike is built as a `.app` bundle with an `Info.plist` (part 01 reads the identifier from `--app` and rejects an unidentified executable); the coexistence binary is either a `.app` or embeds its `Info.plist` with `-sectcreate __TEXT __info_plist`. The README records how each is built and that `lsappinfo` reports the identifier for a running instance.
- Neither binary reads or writes Stache configuration, control-socket or cache paths (`~/.config/stache`, `$TMPDIR/com.marcosmoura.stache.next/`, the app caches), and neither talks to a running Stache.
- Part 01's tool refuses a spike run while Stache Next, legacy Stache or another spike instance runs, and a `next` run while a spike runs (exact path and PID, never name matching).

### Command-line contract

- `--cadence none|1s` (required) sets the content update cadence.
- `--window none|surface` (required) selects part 01's scenario. Both show the spike's tray icon (matching Stache Next, whose tray exists in both scenarios); `none` shows nothing else, `surface` also shows the Bar-like window.
- Any other or unknown argument exits non-zero with a usage line and opens no window and no tray icon.
- Fixed stderr lines, each written once and printed bare by the spike itself (no logger prefix): `spike: ready` after the tray icon exists (`--window none`) or after the tray icon exists and the window is created and shown (`--window surface`), `spike: first frame presented` after GPUI reports the first presented frame (if GPUI exposes no such callback, the line is not written and the README says so; first frame is then **unverified** for the spike too).

### Root isolation

Effective changes (each verified against the current config when implemented):

- Root `Cargo.toml` `[workspace] exclude` (currently `["_legacy"]`) gains `"spikes"`, and/or the spike declares its own empty `[workspace]`, so root `cargo` commands never resolve it and the spike builds on its own.
- `.oxfmtrc.json` `ignorePatterns` gains `spikes/**` (otherwise root `format:check` reaches spike Markdown/JSON).
- `lint-staged.config.ts` `ignoredDirectories` gains `spikes`: `isRustInput` matches any `.rs` and any `Cargo.toml`, so a staged spike file would trigger root `cargo fmt` and `lint:native`.
- `.taurignore` gains `/spikes/`, so the Tauri dev watcher does not restart on spike edits.

Verified unneeded (no change; listed in [guides/testing.md](../../guides/testing.md) with the reason):

- `tsconfig.json`, `tsconfig.app.json`, `tsconfig.node.json`: explicit `include` lists that do not reach `spikes/`.
- `vite.config.ts`: build and test root is `src/ui`.
- `vitest.scripts.config.ts`: includes only `scripts/**/*.test.ts`.
- `oxlint` in `lint:ui` lints explicit paths (`src/ui scripts` and named configs), so `.oxlintrc.json` needs no entry.
- `stylelint` glob is `src/ui/**`, so `.stylelintignore` needs no entry.
- `spikes/gpui/target/` is already ignored by the unanchored `target/` in `.gitignore`.

Known gaps, recorded in the README and guides/testing.md:

- Root `rust-toolchain.toml` (pinned nightly) and `rustfmt.toml` apply to the spike through cargo's and rustfmt's upward search unless the spike adds its own.
- CI `cargo audit` runs on the root `Cargo.lock` only; spike dependencies are unaudited.
- The spike is otherwise ungated: no root lint, format or test covers it. Its README carries manual `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` commands run from `spikes/gpui/`; this is acceptable for a throwaway spike.

Dev-watch check: one manual, owner-approved `bun run tauri:dev` session modelled on guides/testing.md "Native dev-watch acceptance": with the dev session running, touch a spike `.rs` file and confirm no recompile or restart, then confirm a harmless `src/crates/app` edit still restarts it. The outcome is recorded in Evidence.

### Bar-like window

One window built with GPUI that matches part 01's measured surface size and placement and runs with part 01's recorded cadence (decision 4). Each item below is recorded as **works**, **works with patch** (describe the patch or raw `NSWindow`/objc2 access used), **blocked** (with the reason) or **not observable (hardware unavailable)** (with the reason; allowed only for items that need hardware the owner does not have, such as a second display):

1. Non-activating panel: clicking it does not activate the process or take key focus from the frontmost app.
2. All Spaces + stationary collection behavior; not shown in Mission Control as a normal window.
3. Custom window level, specifically `BelowNormalWindowLevel` (Bar Decision 3).
4. Click-through region versus clickable controls.
5. Blur/vibrancy behind a transparent, undecorated, shadowless window.
6. Display placement: exact frame on `NSScreen.screens[0]` with a second display connected, a non-zero origin and a different scale; repositioning on topology change.
7. Keeps its frame across Space switches and fullscreen app transitions.
8. Accessory activation policy (no Dock icon).
9. Tray icon: whether GPUI offers one, otherwise an `NSStatusItem` created through objc2 inside the GPUI app, with a menu that triggers a GPUI action on the UI thread.
10. Coexistence: the separate `com.marcosmoura.stache.spike.coexist` binary links both GPUI and Tauri (at the root's exact pins) and tries to run both in one process. Record what happens (compile failure, panic, run-loop starvation, one side never drawing, works) and the process model a migration would therefore need (single toolkit, or GPUI process plus Tauri process).
11. Hover and mouse tracking while the panel is not key and the app is not active.
12. Accept-first-mouse: the first click on a control acts without a prior activating click.
13. No `fullScreenAuxiliary` collection behavior is set ([Bar 03](../2026-11-03-bar/03-panel-window.md) forbids forcing it); record whether GPUI sets it by default.
14. Legacy visual-parity probe: font rendering of one legacy Bar text style (font, size, weight from `_legacy/app/ui/renderer/bar/**` sources), one legacy SVG icon path and the blur material, compared with owner-supplied legacy screenshots (legacy is not launched; see [Bar 04](../2026-11-03-bar/04-ui-shell-parity.md) for the reference rules). This is a feasibility probe, not a parity gate.
15. Accessibility: whether VoiceOver can reach and announce the window's text and one control; a missing accessibility tree is recorded as a migration risk.

### Measurements

Run part 01's tool against the spike's release `.app` (`--app`, readiness `stderr-line:spike: ready`, `--cadence` equal to part 01's recorded cadence) in both scenarios (`no-window` → `--window none`, `surface-visible` → `--window surface`), under the same quiet-system precondition and helper attribution (GPUI uses no WebKit, so any new WebKit helper excludes the trial under part 01's exclude-and-replace rule; surface detection and warmup start use the `spike: ready` line), with the same hardware, size, placement, warmup, duration and trial count (≥ 5 per scenario) as the accepted part 01 run, and record the results under this spec's `measurements/`. If part 01 recorded cadence `none`, the comparable run uses `--cadence none`; a `--cadence 1s` run may be added as a separately labelled extra scenario that part 03 does not compare against the static baseline.

## Acceptance criteria

- [ ] GE-02-01: `spikes/gpui/` builds in release with its pinned dependency; the README records the dependency choice and build facts above with their commands and output. Spike and coexistence binaries use `com.marcosmoura.stache.spike.gpui` and `com.marcosmoura.stache.spike.coexist`, product names without "Stache", and a readable identity (the spike as a `.app`, the coexistence binary as a `.app` or with an embedded `Info.plist`). Both read no Stache config, socket or cache path; the spike implements `--cadence none|1s` and `--window none|surface` (tray icon in both), rejects unknown arguments and prints the fixed stderr lines.
- [ ] GE-02-02: With the spike present and built (its `target/` populated), the root checks that CI runs in `.github/workflows/quality.yml` pass: `bun run check:generated`, `bun run lint`, `bun run format:check`, `bun run test`, `bun run test:ui:coverage`, `bun run build` and `bun run tauri:build`. `git status` shows no spike build output; `cargo metadata` at the root lists no spike package.
- [ ] GE-02-03: All fifteen checklist items have a recorded outcome from an owner-approved run, each with how it was observed; none is left blank or inferred from documentation alone. **Not observable (hardware unavailable)** is accepted only with its reason.
- [ ] GE-02-04: An owner-approved measurement run at part 01's cadence produced `report.md` and `samples.json` for both `no-window` and `surface-visible` scenarios, comparable to part 01 (same method fields), linked from the Evidence section; any extra-cadence scenario is labelled as such.
- [ ] GE-02-05: Root ignore changes, the verified-unneeded list and the known gaps are in [guides/testing.md](../../guides/testing.md); the owner-approved dev-watch check is recorded; `AGENTS.md` names `spikes/` per OQ-3.

## Tests

Root checks above, run with the spike built. Spike-internal tests are optional; argument parsing may have a unit test. The checklist outcomes and measurement samples are the evidence. No root test depends on the spike.

## Evidence

None yet. Unverified until the approved runs.

## Notes

- Rollback: delete `spikes/` and revert the root ignore entries.
- The coexistence experiment must not change the root lockfile; it resolves its own `Cargo.lock` inside the spike.
- A "blocked" item is a valid result, not a reason to patch shipped code or widen scope.
