---
status: in-progress
---

# 02 — Media adapter provenance

## Scope

Make every vendored file in `src/crates/app/resources/media-control/` traceable to a pinned,
content-addressed upstream artifact, and keep it verifiable, per Decisions 1 and 2 (SC-001).
Ship the upstream licence file. Correct the stale "no nested code" statements in the release
spec on `main` (Decision 3). Do not change how the adapter is run or resolved.

## Requirements

- **Pinned source.** Add `scripts/media-control/manifest.json`. It stays outside the resource
  directory so it does not ship in the bundle. It records:
  - the Homebrew formula (`media-control`, version `0.7.7`), the bottle tag
    (`arm64_tahoe`, or `arm64_golden_gate` under Decision 2's alternative) and the blob
    digest;
  - upstream `ungive/media-control` tag `v0.7.7` at revision
    `3cfd5dcf78e7a619f7a42a3e2f29b06eb41027ea`;
  - upstream `ungive/mediaremote-adapter` tag `v0.7.7` at revision
    `e3ff5021eb0875858bd05f48d2e9ba2e962d1cf6`;
  - for every committed file: its path in the repo, its path inside the bottle
    (`media-control/0.7.7/lib/media-control/...`,
    `media-control/0.7.7/Frameworks/MediaRemoteAdapter.framework/...`), its SHA-256 and its
    mode;
  - the three framework symlinks (`Versions/Current -> A`, top-level `MediaRemoteAdapter`,
    `Resources`) with their targets;
  - the upstream `LICENSE` path and SHA-256 at the pinned `mediaremote-adapter` revision.
- **Committed bytes (Decisions 1, 2).** Replace the script, test client, framework binary,
  `Info.plist` and `CodeResources` with the bottle's files, byte for byte, with no
  `install_name_tool` and no re-signing. Expected hashes for `arm64_tahoe`, observed
  2026-11-11:
  - script `d97802e4…a85f`, unchanged;
  - test client `017398fb59840f86a158fa37b83c9c357281a4942b39175c7e97de4c62c97f6c`;
  - framework binary `94c0f4271683b38f741087b8065e9983adbeab48a23102530c90f2a3da6f8de0`;
  - `Info.plist` and `CodeResources`, unchanged.

  If the downloaded bottle differs, stop and report; do not commit unexplained bytes. Under
  the alternative: keep today's bytes, record the framework binary as "bottle file + install
  name `/opt/homebrew/opt/media-control/...` + Homebrew's ad-hoc re-sign", and verify it by
  comparing both files after `codesign --remove-signature` in a temporary copy.

- **Licence.** Add the upstream `LICENSE` from `mediaremote-adapter` at the pinned revision as
  `src/crates/app/resources/media-control/LICENSE`, so it ships in the bundle as BSD-3-Clause
  clause 2 requires. Drop the reconstructed licence text from `PROVENANCE.md`. Keep
  `PROVENANCE.md` as the human-readable record: source chain, bottle, how to verify, how to
  refresh, the legacy import history (`e0b4648`) as history, the arm64-only rule, the minimum
  macOS of the binaries, and the upstream origin of the strict-signature failure.
- **Tool.** Add `scripts/media-control/vendor.ts`, run as
  `bun run vendor:media-control -- <mode>`, with three modes:
  - `--check` (offline, default): committed files, modes and symlinks equal the manifest; no
    extra file exists in the resource directory except `PROVENANCE.md` and `LICENSE`; every
    Mach-O is a thin arm64 image (check the header bytes, no `otool` needed).
  - `--verify-upstream` (network): fetch an anonymous pull token from `ghcr.io`, download the
    bottle blob by digest into a fresh empty temporary directory, and require the downloaded
    SHA-256 to equal the manifest digest before extracting anything. Extract with the system
    `tar`, refusing absolute paths and `..` entries. Compare every manifest file byte for byte,
    and the symlinks by target. Compare the script and `LICENSE` with
    `raw.githubusercontent.com/ungive/mediaremote-adapter/<revision>/...` at the pinned
    revision SHA, not the tag. Exit non-zero on any difference, naming the file.
  - `--refresh --bottle <tag> --version <v>` (network, writes): resolve the formula, bottle
    digest and source revision from `formulae.brew.sh` and the `mediaremote-adapter` submodule
    revision from the GitHub API, then rewrite the files and the manifest. The
    next refresh uses this mode. Its output still has to pass `--verify-upstream` and the live
    probe below before it is committed.

  Downloaded data is untrusted: never execute it, never run an interpreter from inside the
  extraction directory, and delete the directory afterwards.

- **Wiring.** `scripts/media-control/vendor.test.ts` runs `--check` logic in
  `bun run test:scripts`, so hooks and CI catch a modified file. `quality.yml` gets a
  `bun run vendor:media-control -- --verify-upstream` step, written to Part 01's pinning rules.
  Hooks never touch the network.
- **Runtime unchanged.** `media_resources.rs` keeps working without changes: the script, the
  framework directory with `MediaRemoteAdapter` inside, and an executable test client. The
  existing `media_resources` tests and the Bar media tests pass unchanged. The flattened
  bundle layout (`docs/features/bar.md`) stays valid.
- **Release spec facts (Decision 3).** On `main`, in `docs/specs/2026-11-03-release/`:
  - `00-overview.md`: replace "This tree has none of those resources" with the current fact:
    the app bundles `Contents/Resources/media-control/` holding two Mach-O files plus a perl
    script, ad-hoc signed by upstream's build, with the framework failing strict
    verification at source.
  - `01-build-metadata.md`: the Bundle contents rule expects exactly the manifest's
    `media-control` files (plus `PROVENANCE.md` and `LICENSE`) and no other framework. The
    Linkage rule applies to `Contents/MacOS/stache` only and no longer claims there is no
    nested code.
  - `03-signing-notarization.md` Notes: replace the stale bullet with the nested-code
    inventory, the requirement that each nested Mach-O be signed individually, inside out,
    with the Developer ID, hardened runtime and timestamp before the app is signed, and a note
    that Tauri flattens the framework symlinks.

  Change no other requirement, acceptance ID or status there. Under the alternative, add only
  a "See also" line in this part and leave the release spec untouched.

- **Docs.** `docs/features/bar.md` "Media adapter packaging" names the bottle, the manifest,
  both check modes, the binaries' minimum macOS and the licence file. `docs/guides/testing.md`
  names `--check` (in `test:scripts`) and `--verify-upstream` (CI only).

## Acceptance criteria

- [ ] `scripts/media-control/manifest.json` records the formula, bottle tag, blob digest, both
      upstream revisions, every file's in-bottle path, SHA-256 and mode, the symlinks and the
      `LICENSE` hash.
- [ ] Every committed adapter file is byte-identical to the pinned bottle's file, or, under
      Decision 2's alternative, equal after the recorded relocation and signature removal.
- [ ] `otool -l` on both Mach-O files shows `minos 26.0` (tahoe), or the alternative is
      recorded with a "Media needs macOS 27" statement in `bar.md` and `PROVENANCE.md`.
- [ ] `LICENSE` from upstream ships in the resource directory, and `PROVENANCE.md` no longer
      carries reconstructed licence text.
- [ ] `bun run vendor:media-control -- --check` passes, and fails with the file named after a
      one-byte change, a mode change, a removed symlink or an extra file in the directory.
- [ ] `bun run vendor:media-control -- --verify-upstream` passes against the network, and fails
      on a digest mismatch, a file mismatch and a tar entry that escapes the directory.
- [ ] `quality.yml` runs `--verify-upstream`, and Part 01's pin test passes.
- [ ] `media_resources` and Bar media tests pass unchanged.
- [ ] The release spec's stale statements are corrected as specified, with no other change
      there, or the cross-reference alternative is in place.
- [ ] Live probe, approved by the user for the run like the 2026-11-10 feasibility probe:
      `/usr/bin/perl <abs>/mediaremote-adapter.pl <abs>/MediaRemoteAdapter.framework
      <abs>/MediaRemoteAdapterTestClient test` exits 0 from the in-repo copy. A short `stream
      --no-diff` emits at least one data line with media playing. Both are then repeated from
      the bundle produced by `bun run tauri:build`, which closes bar.md's "bundled copy
      unverified". The Stache app itself is not launched.
- [ ] `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- `scripts/media-control/vendor.test.ts`, offline: the real directory against the real
  manifest. The negative cases run on a temporary copy: changed byte, lost exec bit, missing
  or retargeted symlink, extra file, non-arm64 and fat Mach-O headers built in the test, and a
  manifest/file count mismatch. The verifier gets an injected fetch and a synthetic bottle
  tarball built in the test: digest mismatch refused before extraction, `../` and absolute
  entries refused, a symlink entry pointing outside refused, file mismatch, script mismatch
  with upstream.
- End-to-end: `--verify-upstream` against the real ghcr.io bottle and raw GitHub, a real
  `bun run tauri:build`, a hash comparison of `Contents/Resources/media-control/` against the
  manifest (the flattened copies hash like their targets), and the approved live adapter probe
  from both copies.

## Notes

- Homebrew bottles carry build-provenance attestations. If `gh attestation verify <bottle>
  --repo Homebrew/homebrew-core` works without new credentials, record its result in
  `PROVENANCE.md`. It is optional evidence, not a criterion.
- The strict-signature failure is upstream's and stays until release Part 03 re-signs the
  nested code. Do not ad-hoc re-sign here: that would break byte identity with the bottle.
- Some harness sandboxes leave an empty `.claude/` directory inside the resource directory.
  `--check` reports it as an extra entry. Delete it locally; it is untracked.
- Network hosts the tool needs: `formulae.brew.sh` and `api.github.com` (refresh only),
  `ghcr.io` (token and blob; it may redirect to `pkg-containers.githubusercontent.com`) and
  `raw.githubusercontent.com`.
