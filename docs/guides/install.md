# Installing Stache Next

Read when installing, upgrading or removing the app, or moving off the old Stache.

**No release has been published yet.** This guide describes the planned install once one is. Until
the planned rename, the app is called `Stache Next` with the bundle identifier
`com.marcosmoura.stache.next`; the names below use those and will change with the rename.

## Requirements

- A Mac with Apple silicon (arm64). Intel Macs are not supported.
- macOS 26 or later.

## Install

Pick one of the three. Each gives you the same app.

- **DMG:** open the `.dmg`, drag `Stache Next.app` onto `Applications`, then open it from
  `Applications`. The first time, macOS asks whether you want to open an app downloaded from the
  internet; that prompt is normal. Leave the download's quarantine in place and answer the prompt.
- **Zip:** double-click the `.zip` and move `Stache Next.app` into `Applications`, then open it.
- **Homebrew:** `brew install --cask marcosmoura/tap/stache`. This also puts the `stache` command
  on your `PATH`. If Homebrew refuses because an app already sits in `Applications`, that is the
  right answer: remove the old app first (see [Legacy removal](#legacy-removal)) and run the same
  command again. Do not override the refusal.

A build made on a developer machine without Apple signing (an "ad-hoc" build) is for testing only;
it is not something to hand out or install on another Mac.

## Upgrade

1. Stop the running app cleanly: choose **Quit** from its menu bar icon. Never force-quit it.
2. Replace the app:
   - DMG or zip: drag the new `Stache Next.app` into `Applications` and replace the old one.
   - Homebrew: `brew upgrade --cask stache`. Homebrew asks the app to quit with a normal quit
     signal, which runs the same clean shutdown as **Quit**.
3. Open the app again. Nothing relaunches on its own.

Your configuration in `~/.config/stache` and the cache survive an upgrade. Shutdown during logout,
restart or power-off is not verified yet, so quit the app yourself before upgrading.

## Coexistence

While the old Stache is still installed:

- `Stache Next` and the old `Stache` can both be installed and running. They use different
  identifiers, caches and runtime folders.
- They share the configuration folder `~/.config/stache` but read different files: the old app
  reads `config.jsonc`; Stache Next reads `global.toml` and one `<module>.toml` per module.
  Neither reads the other's files.
- Do not turn on the same device automation in both. `proxyAudio` and `notunes` would fight.
- After the rename, do not install the renamed release next to the old app: they share an
  identifier, and the new app refuses to start beside it.
- After the rename, a `bun run tauri:dev` session from a checkout runs beside a release without
  conflict; before it, the dev build and a release share the Stache Next identity. The same
  device-automation caution applies.

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
   `_legacy/target` in a checkout, and remove them. Do not build `_legacy/` again after the rename.
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
   `~/Library/Caches/com.marcosmoura.stache`. After the rename this is also the new app's cache, so
   never delete it while the app runs. You can also remove the old self-signed `Stache App`
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
  `ln -s "/Applications/Stache Next.app/Contents/MacOS/stache" /usr/local/bin/stache`
  (any folder on your `PATH` works).

Check it with `stache --help` or `stache cache path`.

## Uninstall

1. Choose **Quit** from the menu bar icon, or run `brew uninstall --cask stache`.
2. If you installed by hand, move `Stache Next.app` to the Trash and remove the link you created
   (`rm /usr/local/bin/stache`).
3. Remove the cache at `~/Library/Caches/com.marcosmoura.stache.next` and, only while the app is not
   running, `$TMPDIR/com.marcosmoura.stache.next`. If you ran debug builds from a checkout, these
   folders are shared with them; remove them only when no development session is running.
4. **Your configuration stays unless you remove it.** `~/.config/stache` (or
   `$XDG_CONFIG_HOME/stache`) is yours and may still be used by the old app. Only if you are sure
   you want it gone: `rm -r ~/.config/stache`.
