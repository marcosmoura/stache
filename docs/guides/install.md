# Installing Stache

Read when installing, upgrading or removing the app, or moving off the old Stache.

**No release has been published yet.** This guide describes the planned install once one is. The
app is `Stache.app` with the bundle identifier `com.marcosmoura.stache`; the downloads are
`Stache_<version>_aarch64.dmg` and `Stache_<version>_aarch64.zip`.

## Requirements

- A Mac with Apple silicon (arm64). Intel Macs are not supported.
- macOS 26 or later.

## Install

Pick one of the three. Each gives you the same app.

- **DMG:** open the `.dmg`, drag `Stache.app` onto `Applications`, then open it from
  `Applications`. The first time, macOS asks whether you want to open an app downloaded from the
  internet; that prompt is normal. Leave the download's quarantine in place and answer the prompt.
- **Zip:** double-click the `.zip` and move `Stache.app` into `Applications`, then open it.
- **Homebrew:** `brew install --cask marcosmoura/tap/stache`. This also puts the `stache` command
  on your `PATH`. If Homebrew refuses because an app already sits in `Applications`, that is the
  right answer: remove the old app first (see [Legacy removal](#legacy-removal)) and run the same
  command again. Do not override the refusal.

A build made on a developer machine without Apple signing (an "ad-hoc" build) is for testing only;
it is not something to hand out or install on another Mac.

## Upgrade

1. Stop the running app cleanly: choose **Quit** from its menu bar icon. Never force-quit it.
2. Replace the app:
   - DMG or zip: drag the new `Stache.app` into `Applications` and replace the old one.
   - Homebrew: `brew upgrade --cask stache`. Homebrew asks the app to quit with a normal quit
     signal, which runs the same clean shutdown as **Quit**.
3. Open the app again. Nothing relaunches on its own.

Your configuration in `~/.config/stache` and the cache survive an upgrade. Shutdown during logout,
restart or power-off is not verified yet, so quit the app yourself before upgrading.

## Coexistence

- The old Stache used the same identifier, `com.marcosmoura.stache`. Do not keep it installed next
  to the new app: if it is running, the new app refuses to start and tells you to quit it from its
  menu bar icon. Remove it first (see [Legacy removal](#legacy-removal)).
- Both read the configuration folder `~/.config/stache`, but different files: the old app reads
  `config.jsonc`; the new app reads `global.toml` and one `<module>.toml` per module.
- A `bun run tauri:dev` session from a checkout is a development build named `Stache Next`
  (`com.marcosmoura.stache.next`). It runs beside the release with its own lock, socket and cache,
  and the release logs one warning when it sees it. Do not turn on the same device automation in
  both: `proxyAudio` and `notunes` would fight.

## Legacy removal

Do these yourself, in order. Nothing does them for you, and nothing stops apps by name.

1. Quit the old app from its own menu bar icon with **Quit Stache**.
2. Check that it stopped, by bundle identifier: `lsappinfo find bundleid=com.marcosmoura.stache`
   should print nothing. Activity Monitor works too. Do not search processes by the name
   `stache`; that also matches the new app.
3. Back up `~/.config/stache` (or `$XDG_CONFIG_HOME/stache`), including `config.jsonc`, and keep
   it.
4. Move `/Applications/Stache.app` to the Trash. Find any other copies of the old app with
   `mdfind "kMDItemCFBundleIdentifier == 'com.marcosmoura.stache'"`, including build output under
   `_legacy/target` in a checkout, and remove them. Do not build `_legacy/` again: it produces an
   app with the same identifier.
5. Remove the old command-line tool: `cargo uninstall stache`. Then run `which -a stache`; a
   leftover Cargo binary would hide the new command.
6. Install the new app and its command, using [Install](#install) and [CLI access](#cli-access).
   Run `which -a stache` and check that it points at the new command.
7. Before opening the new app, run `stache config migrate`. It writes one `<module>.toml` per
   module and leaves `config.jsonc` untouched. If it reports a file that already exists, look at
   both and decide; it never overwrites on its own. Then open the new app and check it behaves as
   you expect. Keep the backup and `config.jsonc` until you are confident.
   If the folder also holds `<module>.jsonc` files from a development build, plain migrate
   converts those instead. Run `stache config migrate --from ~/.config/stache/config.jsonc`
   instead. Add `--force` to that `stache config migrate --from …` command only after checking
   which `.toml` files it would replace.
8. Optional, with the new app quit: delete the old cache and socket in
   `~/Library/Caches/com.marcosmoura.stache`. This is also the new app's cache, so never delete it
   while the app runs. You can also remove the old self-signed `Stache App`
   certificate, and, with both apps stopped and before granting new permissions, reset old privacy
   records with `tccutil reset Accessibility com.marcosmoura.stache`.

## Config migration

- `stache config migrate` only runs when you ask. It converts the old file into per-module `.toml`
  files and leaves the old file as it is.
- It refuses to replace existing files; `stache config migrate --force` replaces them.
- While an old config file exists and `global.toml` does not, `stache config path` and the app's
  startup log print one note naming the old file and the migrate command to run. The note never
  reads or changes the file.

## CLI access

The `stache` command is the app's own executable reached through a link outside the app. Running
the file inside the app directly always opens the app, so use the link.

- **Homebrew** creates the link for you.
- **DMG or zip:** create it yourself, for example
  `ln -s "/Applications/Stache.app/Contents/MacOS/stache" /usr/local/bin/stache`
  (any folder on your `PATH` works).

Check it with `stache --help` or `stache cache path`.

## Uninstall

1. Choose **Quit** from the menu bar icon, or run `brew uninstall --cask stache`.
2. If you installed by hand, move `Stache.app` to the Trash and remove the link you created
   (`rm /usr/local/bin/stache`).
3. Remove the cache at `~/Library/Caches/com.marcosmoura.stache` and, only while the app is not
   running, `$TMPDIR/com.marcosmoura.stache`. Development builds from a checkout use the separate
   `com.marcosmoura.stache.next` folders; the release never reads or removes them, so delete those
   yourself if you no longer develop Stache.
4. **Your configuration stays unless you remove it.** `~/.config/stache` (or
   `$XDG_CONFIG_HOME/stache`) is yours and may still be used by the old app. Only if you are sure
   you want it gone: `rm -r ~/.config/stache`.
