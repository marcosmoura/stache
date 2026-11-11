---
status: done
---

# 03 — Configuration trust model and warnings

## Scope

Write down the trust model the audit described (design point 1): every same-user process is
trusted, and configuration is code. Add non-fatal warnings for configuration that someone
other than the user can write (Decision 6). The control socket, the loader's accept/reject
rules and the watcher do not change.

## Requirements

- **Trust model docs.**
  - `docs/architecture/config.md` gets a "Trust model" section. Configuration files are
    treated as code: `global.toml` `exec_on_startup` and `keybindings.toml` bindings run
    commands as children of the Accessibility-trusted app, and keybindings hot-reload. The
    loader follows symlinks and does not refuse files because of owner or mode; it warns as
    described below.
  - `docs/architecture/ipc.md` "Endpoint safety" states that any process of the same user can
    call every socket method, including `tiling query windows` (window titles) and the window
    and workspace mutations, and that there is deliberately no peer-credential check because
    the `0600` socket in a `0700` directory already excludes other users.
  - `docs/features/keybindings.md` gets one line linking to the trust model.
- **Check.** Add `stache_sdk::config::trust` (new file `src/crates/sdk/src/config/trust.rs`,
  exported from `config/mod.rs`) with a pure-ish function, for example
  `inspect_trust(paths: &ConfigPaths, files: impl IntoIterator<Item = PathBuf>, owner: u32) -> Vec<TrustWarning>`.
  `owner` is injected like `AppPaths::owner()`; production passes
  `rustix::process::geteuid()`. It examines, following symlinks (`fs::metadata`):
  - the configuration directory;
  - every registered configuration file that exists (all catalog files, `global.toml`
    included);
  - for a file that is itself a symlink, the directory containing its canonical target.
    A path is flagged when its owner is neither `owner` nor root (uid 0), or its mode has
    `0o020` or `0o002` set. A missing file is skipped. A metadata error is skipped here, because
    the loader already reports I/O problems. Each path is reported at most once, and the output
    is in a deterministic order.
- **`TrustWarning`** carries the path, the reason (`ForeignOwner { uid }`, `GroupWritable`,
  `WorldWritable`; several may apply) and the mode. It renders as one line:
  `warning: <path> is <reason> (mode 0775, owner uid 501); Stache runs commands from its configuration, see docs/architecture/config.md#trust-model`.
  Use the doc's real wording; no ANSI when piped.
- **`stache config path`** (`src/crates/cli/src/config.rs`) prints the warnings after the
  entries and the stale-JSONC notes, on stdout. Markers and exit status stay unchanged, both
  offline and when a host answers. The check is local and adds no IPC.
- **Host.** At startup, next to the stale-JSONC hint in `src/crates/app/src/host.rs`, the host
  logs one `warn` record listing every warning, and none when there are none. Reloads and
  watcher batches do not repeat it, matching the stale-hint rule.
- **No refusal.** No warning changes `PathState`, `Initial`, `ReloadOutcome`, registry status
  or tray behavior. Under Decision 6's alternative (refuse), stop and re-spec this part; it is
  not a small variation.
- **Symlinks and dotfiles.** A `~/.config/stache` that is a symlink, or holds symlinks, into a
  user-owned dotfiles checkout with ordinary modes (`0755` directories, `0644` files) produces
  no warning.

## Acceptance criteria

- [ ] `config.md` has a Trust model section, `ipc.md` Endpoint safety states the same-user
      reach and why there is no peer check, and `keybindings.md` links to it.
- [ ] `inspect_trust` flags group-writable, world-writable and foreign-owned directories and
      files, and the parent of a symlink's target. It ignores missing files and root-owned
      paths, and reports each path once, in a stable order.
- [ ] `stache config path` prints one warning line per flagged path, with markers and exit
      status unchanged, offline and with a host answering.
- [ ] The host logs exactly one `warn` record at startup when there are warnings, none when
      clean, and none on reload.
- [ ] A dotfiles-style symlinked layout with ordinary modes yields zero warnings.
- [ ] No loader, reload or tray behavior changes; existing config tests pass unchanged.
- [ ] `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- `trust.rs` unit tests on temp directories: clean `0700` and `0755` directories with `0600`
  and `0644` files (nothing); directory `0775` and `0777`; file `0664` and `0666`; a foreign
  owner simulated by passing an `owner` uid that differs from the file's real uid (no chown
  needed); root ownership accepted when `owner` is non-root, which needs a root-owned fixture
  such as `/` or `/usr` used read-only; a symlinked file whose target directory is `0777`
  (flagged) or `0755` (clean); a symlinked configuration directory; a dangling symlink and a
  missing file (skipped); duplicate paths reported once.
- CLI tests in `src/crates/cli/src/config.rs`: warning lines after entries and notes,
  unchanged markers and exit code, no ANSI when piped.
- Host test modelled on `the_stale_jsonc_hint_is_logged_once_at_startup_and_not_on_reload`:
  one record at startup, none after a reload.
- End-to-end scenario (`src/crates/app/tests/`, alongside `toml_scenarios.rs`): a temp home
  with `~/.config/stache` as a symlink into a temp "dotfiles" tree holding `global.toml` with
  `exec_on_startup`, a `keybindings.toml` with bindings and a `tiling.toml`, all `0644` in
  `0755` directories. Run the real `config path` and the real host startup with fakes for
  native parts: zero warnings, all files `(exists, …)`. Then `chmod g+w` the dotfiles
  directory holding `keybindings.toml`: one warning naming that directory, the file still
  accepted and its bindings still active. Then make `global.toml` world-writable: two
  warnings, same markers. The host logs the warnings once at a fresh startup.

## Notes

- Root is accepted as an owner because only root can write a root-owned `0644` file, and root
  can already do anything to the user. This mirrors OpenSSH `StrictModes`.
- Ancestors above the configuration directory and above a symlink target's directory are not
  walked. That would warn about system and home layouts the user does not control and adds
  little: a writable `~` already gives an attacker everything.
- `0o020` is flagged even though macOS users share group `staff`, because group write makes a
  file writable by every other local account in that group.
