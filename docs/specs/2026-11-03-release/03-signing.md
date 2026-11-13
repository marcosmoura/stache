---
status: in-progress
---

# 03 — Self-signed signing

Read when changing how a release is signed. Rewritten 2026-11-13 (Decisions 22 to 25): there is
no Developer ID, no notarization and no stapling. Depends on [Part 02](02-release-pipeline.md).

## Scope

Sign the app and the DMG with the owner's self-signed certificate, so the signature stays the
same from build to build, and remove the paid path that was built before this decision. A
self-signed build is **not notarized**: Gatekeeper blocks its first open on any Mac until the
person approves it. [Part 05](05-install-upgrade-uninstall.md) documents that step.

## Why sign at all

macOS ties privacy grants such as Accessibility to the code signature. An ad-hoc signature is
different for every build, so every update asks for the grant again. A signature from one
certificate has a designated requirement anchored on that certificate, so the grant survives
updates signed with it, on the owner's Mac and on any Mac that installed an earlier build.

## Requirements

- **Modes.** `bun run release` stays ad-hoc and is what CI runs. `bun run release --self-sign`
  signs with the identity named by `STACHE_SIGNING_IDENTITY` (the owner's is `Stache App`). No
  other signing mode exists.
- **Refusals.** `--self-sign` fails, with no fallback to ad hoc, when the variable is missing or
  empty, when it is `-`, or when `security find-identity -p codesigning` does not list that exact
  name. The identity name is never hardcoded in the script.
- **Environment.** The build child keeps the Part 02 scrub, extended to `STACHE_SIGNING_IDENTITY`,
  `GH_TOKEN` and `GITHUB_TOKEN`.
- **Signing.** `codesign --force --options runtime --timestamp=none --sign <identity>` on each
  nested `media-control` Mach-O, then the main executable, then the bundle (Decision 16). The main
  executable and bundle pass `--entitlements` with the committed `Entitlements.plist`; the nested
  files get none. `--deep` is never used to sign. The DMG is signed with the same identity after it
  is built. No call contacts Apple: no secure timestamp, `notarytool`, `stapler` or `spctl`.
- **Verification** on the staged app, the app in the mounted DMG and the app expanded from the zip:
  `codesign --verify --strict --deep`; `codesign -dvvv` shows the identity as the authority and the
  hardened-runtime flag; and `codesign -d -r-` shows a designated requirement anchored on the
  certificate (`certificate leaf`), never on a `cdhash`.
- **Record.** `release.json` records `signing: "self-signed"`, `notarized: false`, and the SHA-256
  fingerprint of the signing certificate, which is public and lets a person compare releases. An
  ad-hoc run keeps recording `ad-hoc`. No output claims Developer ID, notarization or stapling.
- **Fail closed.** A failed signing or verification step exits non-zero and leaves no `SHA256SUMS`.
- **Paid path removed.** Delete the `--sign` and `--dry-run-signing` modes, the notarization,
  stapling and `spctl` steps, the `STACHE_TEAM_ID` and `STACHE_NOTARY_*` inputs,
  `.github/workflows/release.yml`, the `--dry-run-signing` step in `quality.yml`, and their tests
  and docs. `scripts/release/gate.ts` stays: [Part 06](06-release-verification.md) uses it.
- **CI.** `quality.yml` keeps the ad-hoc `bun run release` step, its checksum check and the
  `v*-rc.*` trigger, and holds no secret. CI never signs with a certificate.

## Acceptance criteria

- [ ] `RELEASE-03-11` `--self-sign` refuses a missing or empty identity, `-`, and a name absent
      from the keychain listing, runs no build and never falls back. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-03-12` Signing arguments hold `--options runtime`, `--timestamp=none` and the
      identity, never `--deep`; the order is nested files, executable, bundle, then DMG; entitlements
      go only on the executable and the bundle; and no command is `notarytool`, `stapler` or
      `spctl`. Test: `scripts/release.test.ts`, with fake runners.
- [ ] `RELEASE-03-13` A failure injected at each signing and verification step, including a
      designated requirement that is `cdhash`-based and an authority that is not the identity,
      stops the run with no `SHA256SUMS`. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-03-14` A self-signed `release.json` records `self-signed`, `notarized: false` and
      the certificate fingerprint; the build child's environment has no signing input or token; and
      no output line claims Developer ID, notarization or stapling. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-03-15` The paid path is gone: no `release.yml`, no `--sign` or `--dry-run-signing`,
      and `quality.yml` has the ad-hoc step, the candidate-tag trigger and no secret. Test:
      `scripts/release.test.ts`.
- [ ] `RELEASE-03-16` On the owner's Mac, `STACHE_SIGNING_IDENTITY="Stache App" bun run release
      --self-sign` completes, the checksums verify, and the three verifications above pass on the
      real artifacts. Check: run by the orchestrator and recorded in this file.
- [ ] `RELEASE-03-17` After a second self-signed build replaces the installed app, it starts
      without asking for Accessibility again. Owner's manual check, recorded in this file.

## Withdrawn

`RELEASE-03-01` to `RELEASE-03-10` belonged to the Developer ID, notarization and `release.yml`
design. Decision 24 removed that path, so they no longer apply. The removed code is in the branch
history up to `ba66a08`.

## Tests

Script tests use fake runners and never touch a keychain. The real run in `RELEASE-03-16` uses the
owner's keychain; macOS may ask once to let `codesign` use the key.

## Notes

- Nested code: `Contents/Resources/media-control/` holds `MediaRemoteAdapterTestClient`, the
  `MediaRemoteAdapter.framework` library (twice, because Tauri flattens the framework's symlinks)
  and `mediaremote-adapter.pl`. Each Mach-O is signed as a plain file. The adapter is loaded by
  `/usr/bin/perl`, not by Stache.
- The certificate's private key is the only thing that ties a release to the owner. It stays in the
  owner's keychain and is never exported to CI. If it is lost, the next release has a new signature
  and every user grants Accessibility once more.
