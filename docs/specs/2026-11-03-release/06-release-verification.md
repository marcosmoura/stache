---
status: draft
---

# 06 — Release verification and published limitations

Read when cutting a release or checking what a release may claim. **PLANNED AND GATED.** Depends on
every other part's implementation and records their integrated manual proofs; it does not require
their status to be `done` before recording those proofs. Gated checks need
[Part 03](03-signing-notarization.md)'s credentials and [Part 04](04-identity-switch.md)'s rename.

## Scope

Prove private candidate artifacts on a second machine before merge, define the separate post-merge
publication checks, and state the limits. A check that did not run is reported as not run.

## Requirements

### Candidate, then final

The merge gate needs this part `done` before merging, and a final tag belongs on `main` after merging.
Verification has two explicit stages; a local cask cannot make private draft asset URLs public.

1. **Private artifact checks, before merge.** Tag `v0.26.0-rc.<n>` on `feat/release`. Quality runs
   on that exact SHA through Part 02's candidate-tag trigger; Part 03 creates a draft release.
   Download with authenticated `gh release download` or use the direct artifact files. Check
   checksums, signatures, `spctl`, `stapler`, `Info.plist` and bundle contents. Perform the local
   cask rehearsal below against those downloaded files, not GitHub draft URLs. Record all part
   criteria before marking this part `done`. Candidate evidence explicitly says **public URL
   checks: not run — deferred to post-promotion stage**, never passed.
2. **Public URL checks, after promotion.** Follow the [operational checklist](#post-merge-operational-checklist),
   outside part completion. Online cask audit and install from the public tap, upgrade and uninstall
   run only once the final release assets are public. Do not create a public prerelease to bridge
   these stages. Tagging, promoting and pushing to the tap each need the owner's explicit approval.

### Private cask rehearsal

Serve authenticated candidate downloads over loopback for local test casks, isolated from the public
tap and never committed or pushed. Use successive **test-cask versions** `0.26.0-test.1` and
`0.26.0-test.2`, with the corresponding DMG checksums and local URLs. App versions and the published
cask remain `0.26.0`. Prove Homebrew orders the chosen test versions successively
before the rehearsal; version strings alone are not upgrade evidence. Install the first local test
cask, launch its app, then upgrade to the second.
Record the old/new candidate SHAs, DMG checksums and installed executable checksums; require a
different build and installed checksum after replacement. A same-version skip is a failure, not
upgrade evidence. Record SIGTERM teardown, lock release and socket removal before relaunch, then
uninstall and check configuration preservation. This proves local cask mechanics, **not public
URL reachability or online audit**. Never run `brew audit --cask --online` against a draft URL.

### Gates

- **Source:** `bun run lint`, `bun run format:check`, `bun run test`, `bun run test:ui:coverage`,
  `bun audit`, `cargo audit --target-os macos`, `bun run build`, and `bun run tauri:build`.
  Require a successful Quality run on the exact candidate SHA, not a nearby PR or branch commit;
  the current main-push/PR-only triggers are insufficient until Part 02 adds candidate-tag triggering.
- **Second machine:** Gatekeeper and staple checks on an arm64 Mac running macOS 26 or later that did not
  build the artifact. Use an authenticated browser download of private assets so quarantine applies,
  or authenticate with `gh release download` and explicitly apply and verify quarantine for the
  first-open check. Record the method; never remove quarantine to obtain a pass.
- **No substitutes:** a build success, a signature or a CI pass is not proof of notarization, and a skip is
  never recorded as a pass.

### Published limitations

`README.md` and `docs/guides/install.md` state these before the first release, and the GitHub release notes
repeat them:

- arm64 only; no Intel or universal build. macOS 26 or later.
- No auto-updater: upgrades are manual or through `brew upgrade`.
- No launch-at-login item.
- No automatic configuration migration: run `stache config migrate`.
- No Settings UI: configuration is files.
- Identity: a release is always `com.marcosmoura.stache`; only development builds use
  `com.marcosmoura.stache.next`. A running legacy app shares this release's identifier and makes it
  refuse to start, with no visible dialog when opened from Finder.
- Removing the legacy app is a manual step and is required before installing this release.
- Cmd-Q-style quit, a quit Apple Event and logout do not run the coordinated teardown; tray Quit and
  SIGTERM do. This is the known gap on the [roadmap](../../roadmap.md).
- The app's own path is not a CLI; use the documented `stache` command, which this line describes by the
  route that shipped ([Part 05](05-install-upgrade-uninstall.md#cli-access)).
- Most planned features remain unshipped; the README feature table stays authoritative.

### Docs

- `README.md`: replace "no packaged release yet" with real install routes and the limitations; the
  build-from-source path stays.
- `docs/guides/testing.md`: replace the "distribution signing/notarization needs separate credentials"
  line with the shipped state and the commands.
- `docs/index.md`: the release row points at the shipped guide. `docs/roadmap.md`: mark shipped only what
  verification proved. Materialize the release flow in `docs/guides/release.md`, about 150 lines.

## Acceptance criteria

- [ ] `RELEASE-06-01` Every source gate passes in Quality on the exact candidate tag SHA; record
      run URL and checked-out SHA. Check: `quality.yml` candidate-tag run, not a nearby commit.
- [ ] `RELEASE-06-02` A fresh authenticated download passes `shasum -a 256 -c SHA256SUMS`, and
      `release.json` states Developer ID, notarized and the tag's commit. Strict signature checks,
      authority/team/runtime/timestamp, merged `Info.plist`, contents and linkage match Parts 01–03
      for both containers. Gated manual check on the second machine.
- [ ] `RELEASE-06-03` On the second machine, offline after download, `spctl` accepts the app and the DMG,
      `stapler validate` passes, and first open shows only the normal downloaded-app prompt. Gated manual
      check; requires credentials.
- [ ] `RELEASE-06-04` On the second machine, DMG and zip installs each launch, create the tray, open the
      test window, and quit from the tray, covering `RELEASE-01-07` and `RELEASE-02-09`. Gated manual check.
- [ ] `RELEASE-06-05` The private local test-cask rehearsal installs the app and selected CLI route,
      upgrades between distinct test-cask versions and candidate builds, proves bundle replacement
      with changed installed checksums (no skip), and records SIGTERM clean teardown
      (`RELEASE-05-08`). Local uninstall leaves `~/.config/stache` untouched. Record the full
      legacy-removal/install/migration rehearsal for `RELEASE-05-09`. Online audit and public tap
      checks are explicitly not run at this stage. Gated manual check; touches `/Applications`.
- [ ] `RELEASE-06-06` With `/Applications/Stache.app` already present, the private local test-cask
      install refuses and leaves it unchanged. Gated manual check, using a harmless placeholder
      bundle, never the real legacy app.
- [ ] `RELEASE-06-07` `README.md` and `docs/guides/install.md` contain every limitation above. Test: a doc
      check in `scripts/release.test.ts`, keyed to the exact phrases.
- [ ] `RELEASE-06-08` A release record in this file lists the candidate tag, exact SHA, Quality run,
      artifact and installed checksums, notarization submission identifiers, download/quarantine
      method, machine and macOS version, and each gated result. Required private checks must pass;
      only the unselected CLI-route criterion is not applicable with reason. Public URL checks are
      listed separately as not run at the stage boundary. Gated manual check, reviewed by the orchestrator.
- [ ] `RELEASE-06-09` Before merge, `docs/guides/release.md` contains the separate operational
      checklist below, and the orchestrator reviews its promotion order, approvals, public-URL
      checks and failure handling against the rendered production cask (`0.26.0`, final URL and
      checksum shape, no test-only overrides). No post-merge action is needed to satisfy this
      criterion. Check: doc test and pre-merge readiness review.

## Post-merge operational checklist

This is release operation, **not an acceptance criterion or a condition of part completion**.
Materialize it in `docs/guides/release.md` before merge; record execution results there afterward.

1. With approval, tag `v0.26.0` on `main`; require exact-SHA source gates and a new final draft.
2. Authenticate and download those final bytes. Repeat private artifact, second-machine first-open,
   DMG/zip install and local cask replacement/teardown checks above, since a rebuild changes checksums.
   Do not reuse candidate evidence as a pass for final bytes. Failure blocks promotion.
3. With approval, promote the final draft to the first public release (not a prerelease). Verify
   unauthenticated download of each final URL and its checksum. Only now begin public URL checks.
4. Render the production cask with real version `0.26.0`, public DMG URL and final checksum, never
   local test versions/URLs. Run `brew audit --cask --online` against it. Failure blocks tap publication.
5. With approval, publish the tap change; on the second machine run
   `brew install --cask marcosmoura/tap/stache`, launch and exercise the selected CLI, then uninstall
   to reset for the upgrade check. For this first release, install the earlier candidate's local
   test cask, then upgrade using a successive local test-cask version pointing at the now-public
   final DMG URL and checksum. Retain the same cask token; prove no skip, replacement and SIGTERM
   teardown, then uninstall and verify config preservation. This exercises public downloads without
   publishing test versions or pretending a prior public release exists. Future releases upgrade
   from the prior public version through the tap. Record versions, builds and checksums, not just exits.
6. Record public audit/install/upgrade/uninstall results and public asset checksums separately from
   candidate evidence. Any failure blocks announcement and further publication; report it and correct
   it explicitly, never reinterpret a skip or failed public check as a candidate pass.

## Tests

The only automated test added here is the doc check. Everything else exercises real artifacts and is gated:
no default suite touches `/Applications`, contacts Apple or modifies the tap.

## Notes

- `marcosmoura/homebrew-tap` has a `Formula` directory and no `Casks` directory, so the first change
  creates it.
- A second arm64 Mac on macOS 26 or later is an assumed resource. Without one, `RELEASE-06-03` is not
  proven; a clean local user account is not an equivalent, and must be reported as such.
