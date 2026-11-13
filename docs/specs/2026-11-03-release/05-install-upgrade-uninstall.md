---
status: done
---

# 05 — Install, upgrade, legacy removal and uninstall

Read when changing what a person does to install, upgrade or remove the app. Revised 2026-11-13
(Decisions 22 to 25): the release is a self-signed DMG on GitHub Releases, with no Homebrew route.
Depends on [Part 02](02-release-pipeline.md), [Part 03](03-signing.md) and
[Part 04](04-identity-switch.md).

## Scope

`docs/guides/install.md`, command-line access for an installed app, and the legacy-config note.
Nothing here removes, quits or rewrites anything automatically.

## Requirements

### Install

- **Requirements** stated up front: Apple silicon (arm64) and macOS 26 or later.
- **DMG:** open it and drag `Stache.app` to `Applications`. **Zip:** expand and move it there.
- **First open.** The app is not notarized, so macOS blocks the first open of a downloaded copy.
  The guide gives the supported route only: try to open the app, then System Settings → Privacy &
  Security → **Open Anyway**, and confirm. It says plainly why this happens. It never recommends
  `xattr -d`, `xattr -cr` or `spctl --master-disable`.
- **Accessibility.** The app asks for Accessibility on first launch. Because every release is
  signed with the same certificate, an update keeps the grant.
- **No Homebrew.** The guide says there is no cask or formula.

### Upgrade

- Quit from the tray, replace the app with the new one, open it again. Nothing relaunches by itself.
  The first open of each downloaded update needs the same approval.
- Tray Quit, Cmd-Q and SIGTERM are verified to run the teardown. Do not rely on logout, restart or
  shutdown, which are not verified ([roadmap](../../roadmap.md)).
- Config and cache survive; the runtime directory is recreated at launch.

### Coexistence

- A release (`com.marcosmoura.stache`) and a development build (`com.marcosmoura.stache.next`) are
  siblings with separate locks, sockets and caches. They read the same configuration directory, so
  do not enable the same device-changing automation in both: `proxyAudio` and `notunes` would fight.
- A release warns in its log when a bundled `Stache Next.app` is running. An unbundled
  `bun run tauri:dev` run has no bundle identifier and is not detected.
- A local `--release` build is not a sibling. It is the release identity, so its host exits as a
  second launch while the release runs.
- An old Stache from before 0.26 shares the release identifier. The new app refuses to start beside
  it; remove it first.

### Legacy removal, explicit and user-run

For anyone who still has the pre-0.26 Stache. Never automated, never by process name. In order:

1. Quit the old app from its own tray with **Quit Stache**.
2. Confirm it stopped by bundle identifier, not name (`lsappinfo` or Activity Monitor). A search by
   the name `stache` also matches the new app.
3. Keep `~/.config/stache` (or `$XDG_CONFIG_HOME/stache`), including `config.jsonc`; a backup is
   recommended.
4. Move the old `/Applications/Stache.app` to the Trash.
5. Remove the old command-line binary: `cargo uninstall stache`. Check `which -a stache` afterwards.
6. Install the new app and link the command line ([CLI access](#cli-access)).
7. Before the first launch, run `stache config migrate`. It writes per-module `<id>.toml` files and
   leaves `config.jsonc` unchanged, and refuses to overwrite existing files. If the directory also
   holds per-module `<id>.jsonc` files, run `stache config migrate --from <path to config.jsonc>`
   (add `--force` only after checking which `.toml` files it would replace).
8. Optional, with the new app quit: clear leftover files in
   `~/Library/Caches/com.marcosmoura.stache` with `stache cache clear`, and reset a stale
   Accessibility record with `tccutil reset Accessibility com.marcosmoura.stache` before granting
   it again.

### Config migration

- `stache config migrate` remains explicit. Discovery follows `ConfigPaths::legacy_sources`.
- **Legacy-config note (shipped).** When a legacy source exists and `global.toml` does not,
  `config path` and the host startup log print one `note:` line naming the source and
  `stache config migrate`, or `stache config migrate --from <source>` when per-module `<id>.jsonc`
  files are also present. Detection uses paths only.

### CLI access

A bundled executable always selects desktop mode, so the app's own path is not a command line.
The route is a symlink outside the bundle, which the [probe](#probe-record) proved selects CLI mode:
`ln -s /Applications/Stache.app/Contents/MacOS/stache <a directory on PATH>/stache`. The guide
gives that command and says to remove the link on uninstall. No separate CLI binary ships.

### Uninstall

1. Quit from the tray.
2. Move the app to the Trash and remove the `stache` link.
3. Remove the cache at `~/Library/Caches/com.marcosmoura.stache` and, while the app is not running,
   `$TMPDIR/com.marcosmoura.stache`.
4. **Configuration is opt-in.** `~/.config/stache` is yours. The guide warns before giving the
   removal command.

## Acceptance criteria

- [x] `RELEASE-05-03` The legacy-config note appears when a legacy source exists and `global.toml`
      does not, is absent otherwise, and leaves the source and directory byte-identical. Test:
      `cli/tests/commands.rs`.
- [x] `RELEASE-05-04` The desktop startup path emits that line without reading the source. Test:
      `app/tests/legacy_config_hint.rs`.
- [x] `RELEASE-05-05` `config migrate` behavior is unchanged. Test: `cli/src/migrate.rs`, unedited.
- [x] `RELEASE-05-06` `docs/guides/install.md` contains the sections above in order, including
      **First open** with the Open Anyway route and the not-notarized statement; says there is no
      Homebrew route; and contains none of `killall`, `pkill`, `pgrep stache`, `xattr -d`,
      `xattr -cr`, `spctl --master-disable` or `brew install`. It links from `README.md` and
      `docs/index.md`. Test: a doc check in `scripts/release.test.ts`.
- [x] `RELEASE-05-07` On a self-signed install, `stache cache path` through the link prints the
      release cache path without starting a host, and a command through the link is answered by the
      running app without a second instance. Manual check, recorded below. The probe half is kept
      proven by `cli/tests/symlink_probe.rs`.
- [x] `RELEASE-05-09` The legacy app, its bundle and its Cargo-installed binary are gone from the
      release machine and the configuration is intact. Recorded in the
      [Stage 2 record](04-identity-switch.md#stage-2-record).
- [x] `RELEASE-05-11` No cask remains: `scripts/release/cask.ts`, its tests and the `Casks` output
      are removed, and `release.json` and `SHA256SUMS` are unchanged by that. Test:
      `scripts/release.test.ts`.

## Withdrawn

`RELEASE-05-01`, `05-02` and `05-08` covered the Homebrew cask, which Decision 24 removed.
`RELEASE-05-10` covered a separate CLI binary; the symlink route was selected, so it never applied.

## Probe record

2026-11-13, macOS 27, `feat/release`: the symlink probe passed. A helper copied to
`<tmp>/Probe.app/Contents/MacOS/stache` reported `is_app_bundle=true` when run directly and
`is_app_bundle=false` with `current_exe` equal to the link path when run through a link outside
the bundle, also when found by name on `PATH`. Mode selection reads `current_exe` without
canonicalizing it. **The symlink route is selected.**

2026-11-13, owner's ad-hoc install of `a744ddf` (Decision 21): `ditto -x -k` of
`Stache_0.26.0_aarch64.zip` into `/Applications`, then
`ln -s /Applications/Stache.app/Contents/MacOS/stache ~/.local/bin/stache`. Through the link,
`stache cache path` printed `~/Library/Caches/com.marcosmoura.stache` and `stache --version` printed
`0.26.0`, with no desktop host started. The owner then launched the installed app and reported that
everything works. With it running (`com.marcosmoura.stache` at `/Applications/Stache.app`),
`stache config path` through the link was answered by the host (every file marked `active`) and no
second instance started. This was an ad-hoc build.

2026-11-13, self-signed install of `a445fa9` (`Authority=Stache App`, hardened runtime, requirement
anchored on the certificate), replacing the ad-hoc copy after the owner quit it: through the same
link, `stache cache path` printed `~/Library/Caches/com.marcosmoura.stache` with no host started.
The owner launched the app and reported it works. With it running, `stache config path` through
the link was answered by the host (files marked `active`) and one instance stayed running.
`RELEASE-05-07` passes.

## Tests

Automated tests use temporary roots and never touch real caches, `/Applications` or a running app.

## Notes

- A self-signed signature does not satisfy Gatekeeper on any Mac, the owner's included, for a
  quarantined download. A copy built or expanded locally is not quarantined and opens directly.
- macOS privacy records follow the identifier and the signature. Records made for the legacy app or
  for an ad-hoc build do not match a self-signed build, so the first self-signed install asks for
  Accessibility once more.
