---
status: draft
---

# 05 — Install, upgrade, legacy removal and uninstall

Read when changing what a person does to install, upgrade or remove the app. **PLANNED.** Depends
on [Part 02](02-release-pipeline.md) and [Part 04](04-identity-switch.md)'s preparation. Its
legacy-removal steps must exist before Part 04's rename. Final shipped-route proof also needs
[Part 03](03-signing-notarization.md)'s signed artifacts and Part 04's rename; Part 06 records
the full private candidate migration and cask rehearsal before completion.

## Scope

Write `docs/guides/install.md`, define the cask, make CLI access real for a released install, and add
the allowed legacy-config diagnostic. Nothing here removes, quits or rewrites anything automatically.
The guide uses `.next` names until the Part 04 rename updates them, and nothing is published before then.

## Requirements

### Install

- **DMG:** open, drag to `Applications`, launch. Gatekeeper's normal first-open prompt for a quarantined
  download is expected. Quarantine is left in place; the guide never recommends `xattr -d`, `xattr -cr`
  or `spctl --master-disable`, and an ad-hoc artifact is described as not installable by the public.
- **Zip:** expand and move to `Applications`.
- **Cask:** `brew install --cask marcosmoura/tap/stache`. If an app already sits at the target,
  Homebrew's refusal is the correct result: remove legacy first, and **never pass `--force`**.
- **Requirements** stated up front: arm64 and macOS 26 or later.

### Cask

`scripts/release.ts` renders `Casks/stache.rb` into `target/release-artifacts/` from the real version and
DMG checksum. Publishing it to the tap is a manual step in [Part 06](06-release-verification.md); this
part pushes nothing. The cask declares:

- `depends_on arch: :arm64` and the macOS 26 requirement, confirmed after promotion by
  `brew audit --cask --online`, which reads `LSMinimumSystemVersion`; draft checks inspect the plist directly;
- `app "Stache.app"` and a `binary` for the CLI, following the route [below](#cli-access);
- `uninstall signal: [["TERM", "com.marcosmoura.stache"]], on_upgrade: :signal`, with **no** `quit:`
  and no `KILL` escalation;
- `zap trash:` for the cache directory only; **`~/.config/stache` is never in `zap`**;
- no `auto_updates`, since there is no updater, and no `preflight`, `postflight` or
  `terminate_process`, which matches by process name.

For private candidate rehearsals only, render a separate local test cask using successive test-cask
versions such as `0.26.0-test.1` and `0.26.0-test.2` and URLs served from authenticated downloads
on loopback. These versions exist only in that local test cask, never in the app, manifests,
release metadata or published cask; the app remains `0.26.0`. Each test cask uses its candidate's
actual DMG checksum. Never publish this test cask or expose a public prerelease.

### Upgrade

- The running app must stop through the coordinated teardown, never a kill. **Manual:** tray **Quit**,
  then replace the app. **Cask:** the `signal:` stanza sends SIGTERM by bundle identifier, which
  converges on the same teardown.
- Do not rely on Cmd-Q, a quit Apple Event or logout: those are the known teardown gap on the
  [roadmap](../../roadmap.md).
- Nothing relaunches automatically; the guide says to launch again. Config and cache survive, and the
  runtime directory is recreated at launch.

### Coexistence, while legacy remains

- `Stache Next` and legacy `Stache` may both be installed and running, since their identifiers, cache and
  runtime directories differ. Their configuration directory is shared, and their files differ:
  legacy reads `config.jsonc`, Stache Next reads `global.jsonc` and per-module files.
- Do not enable the same device-changing automation in both: `proxyAudio` and `notunes` would fight.
- **Do not install the renamed release alongside legacy.** They share an identifier, which is the
  state Part 04 refuses.
- A release and a `bun run tauri:dev` session are expected siblings: separate identifiers, cache and
  runtime directories. A warning is required only for a detectable `.next` bundle, not an
  unbundled dev run with no bundle identifier. The same device-automation caution applies.
- A local `--release` build is not a sibling. It is the release identity, so its host exits as a second
  launch while the release runs. Debug builds, `tauri:dev` and a checkout's CLI use `.next`.

### Legacy removal, explicit and user-run

Public sequence, once migration is approved and the final release is available. Never automated,
never by process name; no public `.next` release is needed. In order:

1. Quit legacy from its own tray with **Quit Stache**.
2. Confirm it stopped by bundle identifier, not name. `lsappinfo` or Activity Monitor's bundle view
   is acceptable; `pgrep stache` is not, since it also matches the new app.
3. Preserve `~/.config/stache` (or `$XDG_CONFIG_HOME/stache`), including `config.jsonc`; make a backup.
4. Move `/Applications/Stache.app` to the Trash. Find other legacy bundles by identifier with
   `mdfind "kMDItemCFBundleIdentifier == 'com.marcosmoura.stache'"`, including archive build output
   under `_legacy/target`, and remove them explicitly. Do not build `_legacy/` again after the rename.
5. Remove the legacy command-line binary installed by `_legacy/scripts/release.sh`:
   `cargo uninstall stache`. Check `which -a stache` afterward; it shadows a newer link on `PATH`.
6. Install the released app and CLI using [Install](#install) and [CLI access](#cli-access): the cask
   installs the chosen CLI route, or a DMG/zip user creates the documented link or copies the separate
   CLI. Confirm `which -a stache` resolves this released route, not a leftover Cargo binary.
7. Before launching the new host, run `stache config migrate` through that released CLI route.
   Resolve collisions explicitly; do not overwrite existing config automatically. Launch the new app
   and verify its behavior. Keep the backup and original `config.jsonc` until confident.
8. Optional, with the new app quit: clean leftover legacy cache and `stache.sock` at
   `~/Library/Caches/com.marcosmoura.stache`. This is now also the release cache path; never delete
   it while the release runs. Optionally remove the self-signed `Stache App` certificate and reset
   stale privacy records with `tccutil reset Accessibility com.marcosmoura.stache` while both apps
   are stopped, before granting any new permissions.

**Owner's pre-rename rehearsal, not the public sequence:** with explicit approval, preserve config
and rehearse explicit migration and new behavior through the development checkout's CLI and `.next`
host, keeping device-changing automation mutually exclusive. Then perform steps 1–5 above and
record the absence of legacy for `RELEASE-04-07` and `RELEASE-05-09`. The rename and private
candidate installation follow; exercise steps 6–8 with that candidate in Part 06. Do not require
a not-yet-published release or the legacy Cargo CLI to perform migration before removal.

### Config migration

- `stache config migrate` remains explicit. It converts a legacy file into per-module files, leaves the
  source unchanged, and refuses collisions without `--force`. Discovery follows
  `ConfigPaths::legacy_sources`.
- **Allowed diagnostic.** When a legacy source exists and `global.jsonc` does not, `config path` and the
  host startup log print one line naming the source and `stache config migrate`. Detection uses paths
  only. It does not parse, copy or rewrite, and it prompts nothing.

### CLI access

A bundled executable always selects desktop, so the released app has no direct CLI at
`<App>.app/Contents/MacOS/stache`. Mode selection does not change under either route below.

**Primary route: symlink.** A link outside the bundle, through the cask's `binary` stanza or a documented
`ln -s` for DMG and zip installs. On macOS `std::env::current_exe` is expected to report the launched
path, so a link outside the bundle should select CLI mode. No run has proven this yet; `RELEASE-05-07`
is what proves it.

**Pre-approved fallback: separate CLI binary.** If the symlink probe fails, ship this without a fresh
decision, and record the probe result and choice in this file:

- A small CLI binary target, installed as `stache` and shipped outside the app bundle. It must stay out
  of the bundle's `Contents/MacOS`, so `RELEASE-01-06`'s one-executable rule still holds; put it outside
  `stache-app`'s `[[bin]]` list if the bundler would otherwise collect it.
- It never starts the desktop host, and it resolves the same identity, lock and socket as the app.
- The DMG and zip carry it beside the app. The cask's `binary` stanza installs it, and the guide documents
  copying it for DMG and zip installs.
- It is signed, notarized and verified like the app under [Part 03](03-signing-notarization.md). A bare
  executable cannot hold a stapled ticket, so its staple check does not apply.

Exactly one route is selected: `RELEASE-05-07` owns the symlink route's pass, or `RELEASE-05-10`
owns the fallback route's pass. Record the unselected criterion as not applicable with the reason,
never as passed. Part completion requires all unconditional criteria plus the selected route's
criterion; this documented conditional applicability is not a skipped required check. Until then,
a standalone debug binary from a checkout reaches only the development host after the rename;
it is a rehearsal tool, not the public migration route.

### Uninstall

1. Tray **Quit**, or `brew uninstall --cask stache`.
2. Move the app to the Trash if installed by hand, and remove any `ln -s` link or copied CLI binary.
3. Remove the cache at `~/Library/Caches/<identity>`, and, only while the app is not running,
   `$TMPDIR/<identity>`. The `.next` directories exist only if you ran a debug build from a checkout; remove
   those only when no development session is active.
4. **Configuration is opt-in.** `~/.config/stache` (or `$XDG_CONFIG_HOME/stache`) is yours and may still
   be shared with legacy. The guide warns before giving the removal command.

## Acceptance criteria

- [ ] `RELEASE-05-01` The rendered cask holds the real version and DMG checksum, `signal:` with
      `on_upgrade: :signal`, `depends_on arch: :arm64`, `app` and `binary`. Local test-cask rendering
      alone permits successive test versions and local URLs; app and published cask stay `0.26.0`.
      Test: `scripts/release.test.ts`, including isolation of the test-only overrides.
- [ ] `RELEASE-05-02` The rendered cask has no `quit:`, `KILL`, `terminate_process`, `auto_updates`,
      flight block or config path in `zap`. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-05-03` The diagnostic appears when a legacy source exists and `global.jsonc` does not, is
      absent otherwise, and leaves the source and directory byte-identical. Test: `cli/tests/commands.rs`,
      with temporary roots.
- [ ] `RELEASE-05-04` The desktop startup path emits that line without reading the source. Test: app test
      with temporary roots.
- [ ] `RELEASE-05-05` `config migrate` behavior is unchanged: source preserved, collisions refused without
      `--force`. Test: existing `cli/src/migrate.rs` tests still pass, unedited.
- [ ] `RELEASE-05-06` `docs/guides/install.md` contains the sections above, and no `killall`, `pkill`,
      `pgrep stache`, `xattr -d`, `xattr -cr`, `spctl --master-disable` or `--force` recommendation. It
      links from `README.md` and `docs/index.md`. Test: a doc check in `scripts/release.test.ts`.
- [ ] `RELEASE-05-07` Record the real symlink probe result: a helper checks whether `current_exe`
      through a link outside the bundle feeds `is_app_bundle` as false without starting a host.
      Only if the symlink is selected, this criterion passes after `stache cache path` and socket
      access work through the shipped link on a real install. If the probe fails, preserve that
      failure evidence, select the fallback and record this criterion as not applicable, not passed;
      `RELEASE-05-10` then carries the shipped-route proof.
- [ ] `RELEASE-05-08` A SIGTERM delivered through the cask's mechanism completes the coordinated teardown:
      the runtime lock releases and the socket inode is removed. Gated manual check in `RELEASE-06-05`'s run,
      since the existing opt-in harness covers SIGTERM only on a directly launched bundle.
- [ ] `RELEASE-05-09` A person following the removal steps on the release machine ends with no legacy
      process, bundle or Cargo-installed `stache` binary, and config intact. The owner's pre-rename
      rehearsal and steps 1–5 are recorded with `RELEASE-04-07`; the full removal/install/migration
      sequence is rehearsed with a private candidate in Part 06 before merge. Gated manual check;
      it touches real user state and needs explicit approval.

- [ ] `RELEASE-05-10` Only if the fallback ships: the rendered cask's `binary` names it, the signing,
      notarization and verification steps include it, and the DMG and zip carry it outside the bundle. Test:
      `scripts/release.test.ts`, with fake runners. On a real install it runs `stache cache path`, reaches
      a running app's socket, and shows the Developer ID authority and hardened runtime. Gated manual check;
      requires credentials. If the symlink route passes, this is recorded as not applicable.

## Tests

Automated tests use temporary roots and never touch real caches, `/Applications` or a running app.

## Notes

- `cargo install --path` in `_legacy/scripts/release.sh` installs the legacy package named `stache` into
  Cargo's bin directory, so a person's `stache` command may be the legacy CLI even after the app is gone.
- `signal:` signals every process matching the bundle identifier. With legacy at the same identifier, that
  would include it; this is one more reason the cask is published only after legacy removal.
