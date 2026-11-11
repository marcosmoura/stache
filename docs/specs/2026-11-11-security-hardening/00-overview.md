---
branch: fix/security-hardening
---

# Security hardening

Read when changing CI pinning, the vendored media adapter, the configuration trust model or
panic-safety claims. Status is in the parts table. Base when written: `main` at `d8366a4`.

## Goal

Close the findings of the 2026-11-11 security audit. The audit found no exploitable
vulnerability in shipped code. It found one supply-chain gap (SC-001, Medium), four CI
hardening items (CI-001..004, Low), two design points and two facts it could not verify.

| Finding                                         | Part                                   |
| ----------------------------------------------- | -------------------------------------- |
| CI-001 mutable action tags                      | [01](01-ci-hardening.md)               |
| CI-002 no `permissions:`, persisted credentials | [01](01-ci-hardening.md)               |
| CI-003 cargo without `--locked`                 | [01](01-ci-hardening.md)               |
| CI-004 unpinned cargo tools                     | [01](01-ci-hardening.md)               |
| Unverified: default `GITHUB_TOKEN` scope        | [01](01-ci-hardening.md) (made moot)   |
| SC-001 media adapter not traceable upstream     | [02](02-media-adapter-provenance.md)   |
| Stale "no nested code" statements, release spec | [02](02-media-adapter-provenance.md)   |
| Design point 1: same-user reach, config is code | [03](03-config-trust.md)               |
| Design point 2: `panic = "abort"` vs guards     | [04](04-release-panic-semantics.md)    |
| Unverified: hardened runtime and nested signing | Release spec Part 03 (cross-reference) |

### Facts established while writing this spec

Checked on 2026-11-11 against the tree, the GitHub API, the Homebrew formula API and the
Homebrew bottles themselves:

- Upstream publishes **source only**. `ungive/mediaremote-adapter` and `ungive/media-control`
  both have releases up to `v0.7.7` (2026-09-03) with **no release assets**. The only
  prebuilt binaries are Homebrew bottles of `media-control` (formula `0.7.7`, built from
  `ungive/media-control` tag `v0.7.7`, revision `3cfd5dcf78e7a619f7a42a3e2f29b06eb41027ea`,
  whose `mediaremote-adapter` submodule is at `e3ff5021eb0875858bd05f48d2e9ba2e962d1cf6`,
  the `mediaremote-adapter` tag `v0.7.7`).
- The committed files trace to the `arm64_golden_gate` (macOS 27) bottle of
  `media-control 0.7.7`, blob `sha256:055b247273a3170f316a9613c1eee3f1e9fb9ee2fae7a1984a0b399dbfa5355c`:
  - `mediaremote-adapter.pl` is byte-identical to the bottle's copy and to
    `bin/mediaremote-adapter.pl` at upstream tag `v0.7.7` (and differs from every earlier tag).
  - `MediaRemoteAdapterTestClient` is byte-identical to the bottle's copy.
  - `MediaRemoteAdapter.framework/Versions/A/MediaRemoteAdapter` differs from the bottle's copy
    only by Homebrew's pour-time relocation: with its install name set to the `/opt/homebrew`
    prefix and both signatures removed (`codesign --remove-signature`), the two files are
    byte-identical. `Info.plist` and `_CodeSignature/CodeResources` are byte-identical.
- The bottle's own framework already fails `codesign --verify --strict` with "code has no
  resources but signature indicates they must be present". The failure comes from the upstream
  build, not from vendoring.
- **New finding:** every committed binary has `LC_BUILD_VERSION minos 27.0`, while
  `tauri.conf.json` declares `minimumSystemVersion` `26.0`. On macOS 26, dyld refuses to load
  them, so Media would be unavailable. The `arm64_tahoe` bottle
  (`sha256:cfc4cf0de19ff242e778ad128ae8426dd9cfceb1219dd6805fd4e00705b9b887`) has `minos 26.0`
  and the same script, `Info.plist` and `CodeResources`.
- **New finding:** the release spec on `main` holds three stale statements, not one:
  `00-overview.md` ("This tree has none of those resources"), `01-build-metadata.md`
  (Bundle contents: "any framework must be absent"; Linkage: "lets Part 03 sign without
  nested code") and `03-signing-notarization.md` Notes ("This tree has no
  `MediaRemoteAdapter.framework` or `resources/` input"). Release Part 01 as written would
  fail against today's bundle.
- `bundle.resources` maps the whole `resources/media-control/` directory, so `PROVENANCE.md`
  ships in the app today and anything added to that directory ships too.
- `tauri build` forwards arguments after `--` to the runner (`cargo build`), so
  `tauri build ... -- --locked` is possible.
- Many more `catch_unwind` sites exist than the three the audit named (about 40 files).
  `docs/architecture/sdk.md`, `docs/features/bar.md` and `docs/features/tiling.md` already
  state that containment works only in unwinding builds; `docs/architecture/ipc.md` does not.

## Non-goals

- Implementing Developer ID signing, notarization or a hardened-runtime check. They stay in
  [the release spec](../2026-11-03-release/00-overview.md), Part 03. This spec only corrects its
  stale facts (Decision 3).
- A peer-credential (`getpeereid`) check on the control socket. The socket is already `0600`
  in a `0700` directory, so a same-UID check rejects nothing those modes do not already reject,
  and nothing can distinguish a trusted same-user client from an untrusted one. The trust
  model is documented instead (Part 03).
- Refusing configuration because of ownership or mode (Decision 6 proposes warnings).
- Auditing code the audit did not cover: most `stache-macos` unsafe, Tiling internals, other
  module runtimes, CLI migrate.
- Anything in `_legacy/`.
- The three informational `cargo audit` warnings (`paste`, `proc-macro-error`, `glib`), all
  transitive.
- Dependency update automation for Cargo or Bun. Only GitHub Actions pins get an update path.
- Building the adapter from source, or changing what the adapter does.

## Decisions

Approved by the user on 2026-11-11, each as recommended. Each states the chosen option first
and the alternative that was not taken after it.

1. **Media adapter source: commit Homebrew bottle files byte for byte, verified by a script.**
   The committed adapter files become the exact files inside one pinned, content-addressed
   `media-control 0.7.7` bottle (no relocation, no re-signing). A manifest records the bottle
   digest, the formula's upstream tag and revisions, and every file's SHA-256. An offline
   check runs in `bun run test`; a network check in CI downloads the bottle by digest, checks
   it and compares every file byte for byte, and compares the script with upstream at the
   pinned commit. The install name stays Homebrew's `@@HOMEBREW_PREFIX@@` placeholder, which
   `dlopen` by path ignores.
   _Alternatives:_ (a) build from the pinned upstream commit with a recorded recipe. Apple
   toolchains do not produce bit-reproducible output, so this only moves the trust to our own
   build and adds Xcode and CMake to CI. (b) fetch the bottle at build time instead of
   committing binaries. This adds a network dependency to every `tauri:build` and to the debug
   fallback in `media_resources.rs`.
2. **Use the `arm64_tahoe` bottle (minos 26.0).** It matches `minimumSystemVersion` `26.0` and
   fixes the macOS 26 load failure. These are new bytes, so the adapter needs one live probe
   on this machine before the part is done.
   _Alternative:_ keep the current `arm64_golden_gate` bytes. Record them as the bottle files
   after Homebrew's relocation and ad-hoc re-sign, verify them by comparing unsigned bytes, and
   document that Media needs macOS 27.
3. **Correct the stale release-spec statements on `main` in Part 02.** Replace the three stale
   facts with the real nested-code inventory and a pointer to the manifest, and change release
   Part 01's "any framework must be absent" to "exactly the manifest's `media-control` files".
   Under [the spec rule](../README.md), `main`'s copy wins when `feat/release` merges, so the
   release worktree must take `main` before it starts Part 01.
   _Alternative:_ leave the release spec untouched and add only a cross-reference from this
   spec. Its owner then fixes it before starting, and Part 01 fails until they do.
4. **`--locked`: add a CI guard, leave the `package.json` scripts alone.** Add one early CI
   step, `cargo fetch --locked`, before any other cargo command, and add a closing
   `git diff --exit-code -- Cargo.lock bun.lock`. Hooks and local runs do not change.
   _Alternative:_ add `--locked` to every cargo-invoking script (`lint:native`, `test:native`,
   `generate-*`, and `tauri:build ... -- --locked`). Every local run is then locked too: after
   a `Cargo.toml` edit, hooks fail until the lock is updated.
5. **Pin upkeep: Dependabot version updates for `github-actions` only.** Weekly, all actions
   in one grouped PR. Dependabot keeps the SHA plus its `# vX.Y.Z` comment current. Cargo tool
   versions in `taiki-e/install-action` are bumped by hand. This is new automation.
   _Alternative:_ no bot. A documented manual refresh in `docs/guides/testing.md`, and the
   pin-format test catches drift back to tags.
6. **Configuration trust: warn, never refuse.** A group- or world-writable or foreign-owned
   (not the current user and not root) configuration directory, registered configuration file,
   or resolved parent directory of a symlinked file produces a non-fatal warning in
   `stache config path` and one host `warn` log at startup. Symlinked and dotfiles-managed
   layouts with normal modes produce nothing. The socket code does not change.
   _Alternative:_ refuse to load such files (treat them like `Invalid`). This is stronger, but
   one stray `chmod g+w` disables the whole module, and the refusal rule must be kept in sync
   with the watcher, reload and tray edits.
7. **Panics: keep `panic = "abort"` and make docs and comments truthful.** Correct
   `ipc.md`, correct the guard comments that promise containment, document what a release
   panic leaves behind, and add a test that pins the release profile's `panic` value so the
   docs cannot drift.
   _Alternative:_ switch release to `panic = "unwind"`. This needs before/after measurement of
   binary size, startup and idle (`bun run measure`). It also changes behavior: a panic
   becomes a contained, degraded state instead of an exit, and every guard path becomes live
   in production.

## Parts

| Part                                 | Summary                                                                                           | Status      |
| ------------------------------------ | ------------------------------------------------------------------------------------------------- | ----------- |
| [01](01-ci-hardening.md)             | SHA-pinned actions, read-only token, no persisted credentials, pinned tools, lock guard           | in-progress |
| [02](02-media-adapter-provenance.md) | Media adapter from a pinned bottle with manifest, offline and upstream checks; release-spec facts | in-progress |
| [03](03-config-trust.md)             | Documented same-user trust model; non-fatal config ownership and mode warnings                    | done        |
| [04](04-release-panic-semantics.md)  | Truthful panic docs and comments under `panic = "abort"`, pinned by a test                        | done        |

Parts are independent except that Part 02 adds a step to the workflow Part 01 hardens, and
Part 01's pin test must cover it. Each commit must pass `bun run lint`,
`bun run format:check` and `bun run test`.

## Affected docs

At implementation, each in the part that makes its statement true:
[testing.md](../../guides/testing.md) (01, 02),
`src/crates/app/resources/media-control/PROVENANCE.md` (02),
[bar.md](../../features/bar.md#media-adapter-packaging) (02),
[release spec](../2026-11-03-release/00-overview.md) `00`, `01`, `03` (02, Decision 3),
[config.md](../../architecture/config.md) (03), [ipc.md](../../architecture/ipc.md) (03, 04),
[keybindings.md](../../features/keybindings.md) (03), [sdk.md](../../architecture/sdk.md) (04).
[index.md](../../index.md) gains a row for this spec when it lands.
