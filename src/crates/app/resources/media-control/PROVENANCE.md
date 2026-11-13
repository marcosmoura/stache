# Vendored MediaRemote adapter

Read when updating, auditing or repackaging the Bar media adapter. Nothing in this
directory is Stache code; it is third-party material shipped unmodified.

- **Upstream:** `mediaremote-adapter` by Jonas van den Berg,
  <https://github.com/ungive/mediaremote-adapter>, tag `v0.7.7` at revision
  `e3ff5021eb0875858bd05f48d2e9ba2e962d1cf6`.
- **License:** BSD 3-Clause. `LICENSE` in this directory is the upstream file at that revision,
  byte for byte, and ships in the app bundle with the binaries.
- **Source chain.** Upstream publishes source only. The binaries come from the Homebrew bottle
  of `media-control` `0.7.7` (built from `ungive/media-control` tag `v0.7.7`, revision
  `3cfd5dcf78e7a619f7a42a3e2f29b06eb41027ea`, whose `mediaremote-adapter` submodule is the
  revision above), bottle tag `arm64_tahoe`, blob
  `sha256:cfc4cf0de19ff242e778ad128ae8426dd9cfceb1219dd6805fd4e00705b9b887`
  (`ghcr.io/homebrew/core/media-control`). Every file here is the bottle's file byte for byte:
  no `install_name_tool`, no re-signing. The framework therefore keeps Homebrew's placeholder
  install name `@@HOMEBREW_PREFIX@@/opt/media-control/...`; the adapter loads it by path, so the
  install name is not used. `mediaremote-adapter.pl` and `LICENSE` also equal
  `bin/mediaremote-adapter.pl` and `LICENSE` at the upstream revision.
- **Manifest.** `scripts/media-control/manifest.json` (outside this directory, so it does not
  ship) records the formula, bottle tag, blob digest, both upstream revisions, every file's
  in-bottle path, SHA-256 and mode, the three symlinks and the `LICENSE` hash.
- **Verify.** `bun run vendor:media-control -- --check` (offline, also in `bun run test:scripts`)
  compares this directory with the manifest: hashes, executable bits, symlink targets, no extra
  entry, thin arm64 Mach-O headers. `bun run vendor:media-control -- --verify-upstream` (network,
  CI) downloads the bottle by digest, checks its SHA-256 before extracting, refuses unsafe
  archive entries, and compares every file and symlink with the bottle and the script and
  `LICENSE` with upstream at the pinned revision. A harness that leaves an empty `.claude/` here
  makes `--check` report an unexpected entry; delete it, it is untracked.
- **Refresh.** `bun run vendor:media-control -- --refresh --bottle <tag> --version <v>` resolves
  the bottle digest and revisions from `formulae.brew.sh` (and the submodule revision from the
  GitHub API), requires the adapter tag to resolve to that submodule revision, reads every
  manifest path from the bottle before writing anything, then rewrites these files and the
  manifest and notes any bottle file the manifest does not list. This file and
  `docs/features/bar.md` must then be updated by hand; a test in
  `scripts/media-control/vendor.test.ts` fails until they name the manifest's version, bottle
  tag, digest, revisions and hashes. The result must pass `--verify-upstream` and an approved
  live adapter probe before it is committed.
- **Architecture:** arm64 only (`MediaRemoteAdapterTestClient` and
  `MediaRemoteAdapter.framework/Versions/A/MediaRemoteAdapter` are thin arm64 Mach-O).
  The app reports `unsupportedArchitecture` on other hosts without spawning anything.
- **Minimum macOS:** both Mach-O files carry `LC_BUILD_VERSION minos 26.0`, matching the app's
  `minimumSystemVersion`.
- **Signatures:** both Mach-O files carry upstream's ad-hoc signatures.
  `MediaRemoteAdapterTestClient` verifies in place. In place, the framework and its binary fail
  even plain `codesign --verify` ("code has no resources but signature indicates they must be
  present"); only a copy of the binary outside the framework verifies. That failure comes from
  the upstream build and is present in the bottle itself. Release signing re-signs the nested
  code.
- **Layout kept:** `Versions/A`, `Versions/Current -> A` and the top-level
  `MediaRemoteAdapter` and `Resources` links are stored in git as symlinks. Tauri's
  `bundle.resources` flattens them in the `.app` (see `docs/features/bar.md`).
- **History:** the first import came from the resources of the application before 0.26,
  committed in `e0b4648` and copied byte for byte. Those
  binaries were the `arm64_golden_gate` (macOS 27, `minos 27.0`) bottle's files after Homebrew's
  pour-time relocation and re-sign; they were replaced with the `arm64_tahoe` bottle's files on
  2026-11-11.

| File                                                         | SHA-256                                                            |
| ------------------------------------------------------------ | ------------------------------------------------------------------ |
| `mediaremote-adapter.pl`                                     | `d97802e46db9535e2549e178c105ebf417a0254b3929fc32f08ecfd14d49a85f` |
| `MediaRemoteAdapterTestClient`                               | `017398fb59840f86a158fa37b83c9c357281a4942b39175c7e97de4c62c97f6c` |
| `MediaRemoteAdapter.framework/Versions/A/MediaRemoteAdapter` | `94c0f4271683b38f741087b8065e9983adbeab48a23102530c90f2a3da6f8de0` |
| `MediaRemoteAdapter.framework/Versions/A/Resources/Info.plist` | `cf7c9699302a8ddacf24465443eb7a0153e1c347bf23c3c0895af41c85368af8` |
| `MediaRemoteAdapter.framework/Versions/A/_CodeSignature/CodeResources` | `5ddd46b76ea756abd4e084c1f21566a39b4ff890bd21dce9b6f11ace69563305` |
| `LICENSE`                                                    | `a489c16a6be195584fa0b35e28fca54abc8492eb0cc05d851dd24f522f4900a2` |

## Feasibility (2026-11-10, macOS 27.0, arm64)

The approved probe ran `/usr/bin/perl <abs>/mediaremote-adapter.pl <abs>/MediaRemoteAdapter.framework
<abs>/MediaRemoteAdapterTestClient stream --no-diff` against the previous (`e0b4648`) files. It
streamed newline-delimited JSON (`{"type":"data","diff":false,"payload":{...}}`), showed
no permission prompt, spawned no child processes (perl loads the framework in-process)
and ended on `SIGTERM` with the child reaped. Relative paths fail ("Failed to load
framework"), so callers must pass absolute paths. Largest observed line was about 157 KB
(base64 artwork).

## Live probe of the current files (2026-11-11, macOS 27.0, arm64)

The approved probe ran the `arm64_tahoe` files from this directory and from the release bundle
built by `bun run tauri:build`. In both places `test` exited 0 with no output, and six seconds of
`stream --no-diff` produced two `data` lines (an empty payload, then the full now-playing payload,
about 193 KB with artwork), nothing on standard error and a clean exit on `SIGTERM`. The bundled
files matched the manifest byte for byte with their executable bits. The binaries are built for
macOS 26 (`minos 26.0`); they were run on macOS 27.0 only, so macOS 26 itself is not verified.
