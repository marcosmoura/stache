# Installing Stache

Read when installing, upgrading or removing the app, or moving off the old Stache.

**No release has been published yet.** This guide describes the install once one is. Releases
will be on the project's GitHub Releases page as `Stache_<version>_aarch64.dmg` and
`Stache_<version>_aarch64.zip`. The app is `Stache.app` with the bundle identifier
`com.marcosmoura.stache`.

## Requirements

- A Mac with Apple silicon (arm64). Intel Macs are not supported.
- macOS 26 or later.

## Limitations

- Apple silicon (arm64) only; no Intel or universal build. macOS 26 or later.
- Not notarized by Apple. The app is self-signed, so macOS blocks the first open until you allow
  it in System Settings.
- No Homebrew cask or formula.
- No auto-updater: upgrades are manual.
- No launch-at-login item.
- No automatic configuration migration: run `stache config migrate`.
- A development build (`Stache Next`) is separate from the release and can run beside it.
- An old Stache from before 0.26 must be removed first; the new app refuses to start beside it,
  with no visible dialog when opened from Finder.
- Tray Quit, Cmd-Q and SIGTERM run the coordinated teardown. Logout, restart and shutdown are not
  verified to.
- The app's own path is not a command line; link `stache` as the install guide describes.

## Install

Download either file; both hold the same app.

- **DMG:** open the `.dmg` and drag `Stache.app` onto `Applications`.
- **Zip:** double-click the `.zip` and move `Stache.app` into `Applications`.

There is no Homebrew cask or formula; GitHub Releases is the only download.

## First open

Stache is not notarized by Apple, because the project does not pay for the Apple Developer
Program. Instead it is signed with the author's own certificate. macOS does not recognize that
certificate, so it blocks the first open of a downloaded copy. Whether to allow it is your
decision.

To open it:

1. Open `Stache.app` from `Applications`. macOS says it cannot verify the app. Dismiss the
   message with **Done**.
2. Open **System Settings** → **Privacy & Security**.
3. Scroll down to the message about Stache and choose **Open Anyway**.
4. Confirm with **Open Anyway** again and enter your password if asked.

You need to do this once for each version you download.

On first launch, Stache asks for **Accessibility** access. Grant it in System Settings →
Privacy & Security → Accessibility. Every release is signed with the same certificate, so an
update keeps this permission.

## Upgrade

1. Choose **Quit** from the Stache menu bar icon.
2. Replace `Stache.app` in `Applications` with the new one.
3. Open the app again. Nothing relaunches on its own. The first open of each new download needs
   the same approval as in [First open](#first-open).

Quitting from the menu bar, Cmd-Q and a normal quit signal all shut the app down cleanly.
Shutdown during logout, restart or power-off is not verified yet, so quit the app yourself before
upgrading. Your configuration in `~/.config/stache` and the cache survive an upgrade.

## Coexistence

- A development build from a checkout is named `Stache Next` (`com.marcosmoura.stache.next`). It
  runs beside the release with its own lock, socket and cache. Both read `~/.config/stache`, so do
  not turn on the same device automation in both: `proxyAudio` and `notunes` would fight.
- The release logs a warning when a bundled `Stache Next.app` is running. A `bun run tauri:dev`
  session has no bundle identifier and is not detected.
- A local `--release` build is not a sibling: it is the release itself, so it exits as a second
  launch while the release runs.
- The old Stache from before 0.26 used the same identifier. The new app refuses to start beside
  it; remove it first (see [Legacy removal](#legacy-removal)).

## Legacy removal

For anyone who still has the old Stache from before 0.26. Do these yourself, in order. Nothing
does them for you.

1. Quit the old app from its own menu bar icon with **Quit Stache**.
2. Check that it stopped, by bundle identifier:
   `lsappinfo find bundleid=com.marcosmoura.stache` should print nothing. Activity Monitor works
   too. Do not search for processes named `stache`; that also matches the new app.
3. Keep `~/.config/stache` (or `$XDG_CONFIG_HOME/stache`), including `config.jsonc`. A backup is
   recommended.
4. Move the old `/Applications/Stache.app` to the Trash.
5. Remove the old command-line tool with `cargo uninstall stache`. Then run `which -a stache` to
   check that no old copy is left.
6. Install the new app ([Install](#install)) and link its command ([CLI access](#cli-access)).
7. Before opening the new app, run `stache config migrate`. It writes one `<module>.toml` per
   module, leaves `config.jsonc` unchanged and refuses to overwrite existing files. If the folder
   also holds `<module>.jsonc` files, run
   `stache config migrate --from ~/.config/stache/config.jsonc` instead. Add `--force` to that
   `stache config migrate` command only after checking which `.toml` files it would replace.
8. Optional, with the new app quit: clear leftover files in
   `~/Library/Caches/com.marcosmoura.stache` with `stache cache clear`, and reset an old
   Accessibility record with `tccutil reset Accessibility com.marcosmoura.stache` before granting
   it again.

## Config migration

- `stache config migrate` only runs when you ask. It converts the old `config.jsonc` into
  per-module `.toml` files and leaves the old file as it is.
- While an old config file exists and `global.toml` does not, `stache config path` and the app's
  startup log print one note naming the old file and the migrate command to run. The note never
  reads or changes the file.

## CLI access

The `stache` command is the app's own executable, reached through a link outside the app. Running
the file inside the app directly always opens the app, so use the link. Create it in any folder on
your `PATH`, for example `~/.local/bin`:

```sh
ln -s /Applications/Stache.app/Contents/MacOS/stache ~/.local/bin/stache
```

Check it with `stache --version` or `stache cache path`. When the app is running, commands
through the link are answered by it.

## Uninstall

1. Choose **Quit** from the Stache menu bar icon.
2. Move `Stache.app` to the Trash and remove the link: `rm ~/.local/bin/stache` (or wherever you
   created it).
3. Remove the cache at `~/Library/Caches/com.marcosmoura.stache` and, only while the app is not
   running, `$TMPDIR/com.marcosmoura.stache`.
4. **Your configuration stays unless you remove it.** `~/.config/stache` (or
   `$XDG_CONFIG_HOME/stache`) is yours and holds settings you may want later. The next command
   deletes it permanently, so run it only if you are sure: `rm -r ~/.config/stache`.
