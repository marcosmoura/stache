---
status: draft
---

# 03 — Signing, notarization and stapling

Read when credentials become available or when changing the signing step. **PLANNED AND
BLOCKED:** no Developer ID certificate or notarytool credentials exist. Depends on
[Part 02](02-release-pipeline.md).

## Scope

Sign the app with a Developer ID Application identity, notarize it, and staple the ticket, so
a downloaded DMG and zip open without a Gatekeeper bypass. Add the tag-triggered workflow that
produces a draft GitHub Release.

The pipeline and its verification may be written and dry-run ad-hoc now. This part **cannot be
`done`, and `feat/release` cannot merge,** until the gate below is satisfied and the real checks
have passed. See the [overview](00-overview.md#blocked-gates-and-merge-consequence).

## Gate

All of these must exist and be confirmed by the owner before any real signing run: a
`Developer ID Application` certificate with its private key, the Apple Developer team
identifier, and notarytool credentials (an App Store Connect API key for CI; an app-specific
password is acceptable locally). The existing `Stache App` and `Apple Development` identities
do not satisfy it, and the script must refuse them.

## Requirements

- **Modes.** `bun run release` stays ad-hoc. `bun run release --sign` requires the gate's
  inputs from the environment and a clean tree. Missing input is an error, never a fall back
  to ad-hoc.
- **Final-signature authority.** The Part 02 environment scrub prevents Tauri's credential-driven
  signing and notarization, not its config-driven initial ad-hoc signature (`signingIdentity: "-"`).
  The script then Developer ID re-signs the bundle Tauri built and is the single authority for
  the final signature and notarization; there is not a single signer across all build stages.
- **Signing.** `codesign --force --options runtime --timestamp` with the Developer ID identity,
  on the single executable and then the bundle. `--deep` is **never** used to sign, so an
  unexpected nested binary fails verification instead of being silently covered. Verification
  does use `codesign --verify --strict --deep`.
- **Ad-hoc dry run.** With `--dry-run-signing`, the same step order runs with identity `-`,
  `--options runtime`, `--timestamp=none`, and stubbed notarization calls that are reported as
  skipped. `release.json` records `ad-hoc` and `notarized: false`.
- **CLI fallback.** If [Part 05](05-install-upgrade-uninstall.md#cli-access) ships the separate
  CLI binary, it goes through this same pipeline; `RELEASE-05-10` owns that proof.
- **Entitlements.** None to start. No shipped module uses Apple Events, the camera, microphone
  capture or screen capture. A signed-build failure that truly needs one requires a recorded
  reason and the narrowest key, not a broad exception.
- **Order.**
  1. Sign the app and verify it.
  2. Zip it with `ditto` for submission, `xcrun notarytool submit --wait`, and require `Accepted`.
  3. Staple the app and validate the staple.
  4. Build the DMG from the stapled app, sign the DMG, notarize it, staple it and validate.
  5. Build the distributable zip from the stapled app, since a zip cannot itself be stapled.
  6. Write `SHA256SUMS` over the final bytes, then `release.json`.
- **On rejection.** Fetch `notarytool log` into the release evidence and fail. Nothing is
  labelled signed or notarized unless the step it names succeeded.
- **Verification** after packaging, on the final artifacts: `codesign --verify --strict --deep`,
  `codesign -dvvv` for the Developer ID authority, team, hardened-runtime flag and timestamp,
  `spctl --assess --type execute -vv` for the app,
  `spctl --assess --type open --context context:primary-signature -vv` for the DMG, and
  `xcrun stapler validate` for the DMG and for the app inside both the DMG and the expanded zip.
- **Workflow.** A new `.github/workflows/release.yml`, separate from `quality.yml`, runs on
  `v*` tags and `macos-26`. It fails unless the tag's version equals the authoritative version.
  `v<version>-rc.<n>` tags produce candidates. Every run creates a **draft** release; promotion
  is manual, in [Part 06](06-release-verification.md). Before signing, require a successful
  Quality run for the exact tagged SHA, enabled by Part 02's candidate-tag trigger (the final
  SHA is covered by the `main` push). Missing, pending or failed source gates block the run.
- **Secrets.** They live only in a protected `release` GitHub environment, reach only the
  signing step, and are never echoed or written outside a temporary keychain and key file that
  an `if: always()` step deletes. `pull_request` events never trigger this workflow.
- **Fail closed.** A signing, notarization, stapling or verification failure uploads nothing
  and leaves no draft.

## Acceptance criteria

- [ ] `RELEASE-03-01` Before the first real run, the gate is recorded in this file with the
      certificate's common name and team identifier. Gated manual check.
- [ ] `RELEASE-03-02` `--sign` refuses missing inputs, a non-Developer-ID identity and a dirty
      tree, with no fallback. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-03-03` Constructed arguments contain `--options runtime` and `--timestamp`, never
      `--deep` when signing, and the step order is exactly as stated. Test:
      `scripts/release.test.ts`, with fake runners.
- [ ] `RELEASE-03-04` A failure injected at each sign, notarize, staple and verify step stops the
      run, writes no `SHA256SUMS`, and never records `notarized: true`. Test:
      `scripts/release.test.ts`.
- [ ] `RELEASE-03-05` `--dry-run-signing` completes locally through the real `codesign` with
      identity `-`, and the hardened-runtime flag shows in `codesign -dvvv`. Check: `quality.yml`.
- [ ] `RELEASE-03-06` The workflow triggers only on `v*`, gates secrets behind the `release`
      environment, requires successful Quality gates for the exact tagged SHA, checks the tag
      against the version and creates drafts only. Test: a parse of `release.yml` in
      `scripts/release.test.ts`, plus missing/pending/failed/wrong-SHA gate cases.
- [ ] `RELEASE-03-07` **Gated, requires credentials.** A real run's app shows the Developer ID
      authority, the team, the hardened-runtime flag and a secure timestamp, and strict deep
      verification passes. Evidence is the workflow log.
- [ ] `RELEASE-03-08` **Gated, requires credentials.** `notarytool` reports `Accepted` for the app
      and the DMG, and `stapler validate` passes for the DMG and for the app in both containers.
      Evidence is the workflow log, with submission identifiers.
- [ ] `RELEASE-03-09` **Gated, requires credentials.** A secret scan of the workflow log and the
      artifacts for the certificate password, the `.p12` contents and the API private key finds
      none, and the temporary keychain and key file are gone after the job. Evidence is the log.
- [ ] `RELEASE-03-10` **Gated, requires credentials.** A signed app run under hardened runtime
      creates its tray, loads the embedded test window and quits from the tray with no new
      entitlement, and `proxyAudio` and `notunes` still start when enabled. Gated manual check,
      with per-run approval for noTunes' real termination behavior.

## Tests

Automated tests cover argument construction, ordering and fail-closed paths with fake runners;
nothing signs or contacts Apple in the default suite. Gatekeeper on a machine that did not build
is `RELEASE-06-03`.

## Notes

- Nested code: `Contents/Resources/media-control/` holds two Mach-O files,
  `MediaRemoteAdapterTestClient` and `MediaRemoteAdapter.framework/Versions/A/MediaRemoteAdapter`
  (listed in `scripts/media-control/manifest.json`), plus `mediaremote-adapter.pl`. Upstream's
  build ad-hoc signs them and its framework already fails strict verification. Each nested Mach-O
  must be signed individually, inside out, with the Developer ID, hardened runtime and a timestamp
  before the app itself is signed. Tauri flattens the framework symlinks in the bundle (the
  top-level `MediaRemoteAdapter` becomes a regular copy of the binary and `Versions/Current` and
  `Resources` are absent), so signing must work on the flattened layout. Verification stays strict
  and deep.
- Apple's notarization documentation could not be retrieved while writing this spec. The zip
  submission and zip-cannot-be-stapled statements come from prior knowledge; `RELEASE-03-08` is
  what proves the sequence.
