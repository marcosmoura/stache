---
status: done
---

# 06 — Publishing and verification

Read when cutting a release or checking what a release may claim. Rewritten 2026-11-13
(Decisions 22 to 25). Depends on every other part.

## Scope

Publish a self-signed release from the owner's Mac as a draft on GitHub Releases, state the
limits plainly, and prove the whole route once with a private candidate before the branch merges.
A check that did not run is reported as not run.

## Requirements

### Publish command

`bun run release:publish` runs `scripts/release/publish.ts`. It publishes what
`bun run release --self-sign` already built and never builds, signs, tags or pushes anything.

- **Preconditions, each a refusal with a clear message:** `target/release-artifacts/release.json`
  exists, records `self-signed`, a clean tree and the current `HEAD` commit, and its checksums
  verify; `HEAD` carries a tag `v<version>` or `v<version>-rc.<n>` whose version equals the
  authoritative version; that tag exists on `origin` at the same commit; the Quality workflow has a
  successful `push` run for that exact commit (`scripts/release/gate.ts`; missing, pending, failed
  and pull-request runs all refuse); and no release exists for the tag.
- **Action:** one `gh release create <tag> --draft --verify-tag`, uploading exactly the DMG, the
  zip, `SHA256SUMS` and `release.json`, with generated notes. A candidate tag adds `--prerelease`.
- **Always a draft.** The command never publishes. Promotion is the owner's manual step.
- **Notes** are generated from the same facts every time: the requirements (Apple silicon, macOS
  26 or later), the install and first-open steps from the install guide, the published
  limitations below, and the signing certificate's SHA-256 fingerprint.

### Candidate before merge

The merge gate needs this part `done`, and the final tag belongs on `main` after the merge. So the
route is proven once with a candidate: push `feat/release` and a `v0.26.0-rc.1` tag, let Quality
pass on that exact commit, build with `--self-sign`, publish the draft, download the DMG **in a
browser** so macOS quarantines it, verify the checksums, and go through the first open. Pushing
the branch and the tag each need the owner's go-ahead. The draft is deleted or left unpublished
afterwards; no public prerelease is made.

### Published limitations

`README.md` and `docs/guides/install.md` state these, and the release notes repeat them:

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

### Docs

- `README.md`: replace "no packaged release yet" with the DMG route and the limitations; the
  build-from-source path stays.
- `docs/guides/release.md`: the two modes, the publish command and the operational checklist below.
- `docs/guides/testing.md`, `docs/index.md` and `docs/roadmap.md`: the shipped state, and only what
  verification proved.

## Acceptance criteria

- [x] `RELEASE-06-10` `release:publish` refuses each failed precondition above, runs no `gh release
      create`, and names the reason. Test: `scripts/release/publish.test.ts`, with fake runners.
- [x] `RELEASE-06-11` On success it runs exactly one `gh release create` with `--draft` and
      `--verify-tag`, uploads exactly the four files, adds `--prerelease` only for a candidate tag,
      and never runs a `git push`, a `git tag` or a command that publishes. Test: same.
- [x] `RELEASE-06-12` The generated notes hold the requirements, the first-open steps, every
      limitation above and the certificate fingerprint. Test: same.
- [x] `RELEASE-06-13` `README.md` and `docs/guides/install.md` contain every limitation above. Test:
      a doc check keyed to the exact phrases.
- [x] `RELEASE-06-14` `docs/guides/release.md` contains the operational checklist below, in order.
      Test: a doc check.
- [x] `RELEASE-06-15` Candidate rehearsal: Quality passes on the exact candidate commit; the draft
      holds the four files; a browser download passes `shasum -a 256 -c SHA256SUMS`; the
      quarantined app is blocked on first open and opens after **Open Anyway**; it then creates its
      tray, opens the popover and quits from the tray (this also covers `RELEASE-01-07` and
      `RELEASE-02-09`). Manual check with the owner, recorded in this file with the tag, commit,
      Quality run, checksums and macOS version.
- [x] `RELEASE-06-16` A release record in this file lists each manual check of Parts 03 to 06 with
      its result, and names anything not run. Reviewed by the orchestrator.

## Release record

Candidate rehearsal, 2026-11-13, owner's Mac, macOS 27.3.

- **Tag and commit:** annotated tag `v0.26.0-rc.1` on `5acb9e57ba33587ff8987322c83770b25db50488`
  (`feat/release` with `main` at `42e0cbb` merged in), pushed with the owner's go-ahead.
- **Quality:** run `43450462246` for that commit. Attempt 1 failed in `bun run test:ui:coverage` on
  one test, `ScrollingLabel (real layout) › enters from the right and is followed by a clone…`, in a
  file this branch does not touch; the same test had passed in the `bun run test` step of the same
  job. Attempt 2, a re-run of the failed job on the same commit, passed every step, including
  `bun run release` and the checksum check, their first run on GitHub. This also confirms
  `RELEASE-02-06` and `RELEASE-02-07` on CI.
- **Build:** `STACHE_SIGNING_IDENTITY="Stache App" bun run release --self-sign` on that commit,
  clean tree. DMG SHA-256 `40637dcd494ad1fdf10f6741732204bb9653f6427969a21a161f64efbabfc4b7`, zip
  `7e20675f90cfd95d5dac105049099770120730b38f6ad3b3f14ac41e1917e2cb`, certificate SHA-256
  `7DD83C5C98F74149043DF084685037C86F5F1ABBB0789D4B090B3C02A500B779`.
- **Publish:** while Quality was running, `bun run release:publish` refused with "Quality run … is
  in_progress". After it passed, one run created a draft prerelease holding exactly the DMG, the
  zip, `SHA256SUMS` and `release.json`. A second run refused with "a release for v0.26.0-rc.1
  already exists". The draft was not promoted.
- **Download:** the owner downloaded the DMG and `SHA256SUMS` in a browser. The file carried
  `com.apple.quarantine`, was byte-identical to the built DMG, and
  `shasum -a 256 -c SHA256SUMS --ignore-missing` printed `Stache_0.26.0_aarch64.dmg: OK`.
- **First open:** the owner opened `Stache.app` from the mounted DMG. macOS blocked it; the owner
  followed the Open Anyway route and reported that it worked.
- **Installed candidate:** the same build, expanded from the zip into `/Applications`, started
  with its Accessibility and location grants intact (`RELEASE-03-17`), and the owner reported it
  works.

Manual checks of Parts 03 to 06:

| Check                                             | Result                                                          |
| ------------------------------------------------- | --------------------------------------------------------------- |
| `RELEASE-03-16` real self-signed build            | Passed, recorded in [Part 03](03-signing.md)                    |
| `RELEASE-03-17` grants survive a second build     | Passed, owner's report                                          |
| `RELEASE-04-07` legacy removal approved and done  | Passed, recorded in [Part 04](04-identity-switch.md)            |
| `RELEASE-04-11` release beside a bundled dev app  | **Not run**, waived by the owner                                |
| `RELEASE-04-13` `tauri:dev` beside the release    | **Not run**, waived by the owner                                |
| `RELEASE-04-14` packaged identifier and CLI path  | Passed                                                          |
| `RELEASE-05-07` CLI link on a self-signed install | Passed                                                          |
| `RELEASE-05-09` legacy gone, config intact        | Passed                                                          |
| `RELEASE-06-15` candidate rehearsal               | Passed, with the limits below                                   |
| `RELEASE-01-07` Finder launch, tray, popover      | Owner's report that the app works; details not itemized         |
| `RELEASE-02-09` drag install from the DMG         | Opened from the mounted DMG; the drag to `Applications` not run |

Not proven: Gatekeeper on a Mac that did not build the artifact; the exact wording of the
first-open dialog, which the install guide describes from memory; a drag install from the DMG; and
the two waived identity checks. No public release exists: the draft is unpublished and the final
`v0.26.0` tag has not been made.

## Withdrawn

`RELEASE-06-01` to `RELEASE-06-09` assumed notarized artifacts, a second Mac and local test casks.
Their source-gate, checksum, install and limitation checks continue as `RELEASE-06-10` to `06-16`.
Gatekeeper on a Mac that did not build the artifact is **not proven** by this spec; the rehearsal
uses a quarantined download on the owner's Mac.

## Operational checklist

Release operation after the merge, **not an acceptance criterion**. `docs/guides/release.md`
carries it.

1. On `main`, with a clean tree, tag `v<version>` and push the tag, with the owner's approval.
2. Wait for Quality to pass on that exact commit.
3. `STACHE_SIGNING_IDENTITY="Stache App" bun run release --self-sign`.
4. `bun run release:publish`, which creates the draft.
5. Download the DMG from the draft in a browser, verify the checksums, and go through the first
   open and a launch.
6. Promote the draft by hand. A failure at any step stops the release; nothing is retried by
   skipping a check.

## Tests

The publish command is tested with fake runners: no test contacts GitHub, builds or signs.
Everything that touches real artifacts, GitHub or `/Applications` is manual and recorded.

## Notes

- `marcosmoura/stache` has no release or tag yet; the candidate is its first tag.
- Quality runs on pushes to `main`, on pull requests and on `v*-rc.*` tags. A final tag on `main`
  is covered by the `main` push run for the same commit.
