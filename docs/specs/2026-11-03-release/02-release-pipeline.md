---
status: in-progress
---

# 02 — Release pipeline

Read when building a distributable artifact or changing the release script. **PLANNED.**
Depends on [Part 01](01-build-metadata.md).

## Scope

One command produces a DMG, a zip, checksums and a truthful record, with predictable names
and no secrets. This part is ad-hoc only, so the pipeline can be proven before credentials
exist. Signing is [Part 03](03-signing-notarization.md); publishing is Part 03's workflow.

## Requirements

- **Command.** `bun run release` runs `scripts/release.ts`, which builds through the existing
  `bun run tauri:build` and works from the resulting
  `target/release/bundle/macos/<productName>.app`. That build's `beforeBuildCommand` already
  runs `check:generated`. `tauri:build` keeps the base config alone; the dev-only overlay from
  [Part 04](04-identity-switch.md#development-identity) never reaches this pipeline.
- **Outputs** in `target/release-artifacts/`, recreated on each run. The directory has no ignore
  rule of its own; it is ignored because `target/` is:
  - `<Product>_<version>_aarch64.dmg`,
  - `<Product>_<version>_aarch64.zip`,
  - `SHA256SUMS`, covering the DMG and zip by bare file name so
    `shasum -a 256 -c SHA256SUMS` works from that directory,
  - `release.json`, recording version, commit, whether the tree was clean, signing mode and
    notarization state.
- **Naming.** `<Product>` is `productName` with spaces replaced by underscores, read from
  the base `tauri.conf.json`, as is the version. No name is hardcoded, so Part 04 changes the
  output from `Stache_Next_…` to `Stache_…` without touching the script. `Stache_Next_…` names
  exist only for ad-hoc dry runs; nothing under that name is ever published.
- **The app keeps its name.** The `.app` inside each artifact stays `<productName>.app` and
  is never versioned: Finder, `/Applications`, the cask's `app` stanza and
  `scripts/measure/process.ts` all rely on that name. The raw bundle is not a distributable.
- **DMG.** Built with `hdiutil` as a compressed read-only image with a fixed volume name,
  holding the app and an `Applications` symlink. No background, Finder scripting or anything
  else requiring the Automation permission.
- **Zip.** Built with `ditto -c -k --keepParent` so signatures and extended attributes survive.
- **Bundle verifier.** Before packaging, assert `RELEASE-01-06`: stated contents, linkage,
  `lipo -archs` reporting exactly `arm64` on the executable and on each nested
  `media-control` Mach-O, no file under `Contents/Resources/media-control/` beyond the
  flattened vendored set, merged `Info.plist`,
  `CFBundleShortVersionString` equal to the authoritative version, and `CFBundleIdentifier` and
  `CFBundleName` equal to the base config's values, so an overlay cannot pass unnoticed. Test
  the artifact rather than the host, since a Rosetta shell can report a different architecture.
- **Fail closed.** Any failed step exits non-zero, leaves no `SHA256SUMS`, and cannot be
  mistaken for success. Write to a staging directory and move it into place only at the end.
- **Honest modes.** Default mode is ad-hoc. Its stdout and `release.json` say `ad-hoc` and
  `notarized: false`, and the DMG and zip names carry no marker that implies otherwise.
  Nothing in this mode reads a credential.
- **Environment.** The build step removes `APPLE_SIGNING_IDENTITY`, `APPLE_CERTIFICATE`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`,
  `APPLE_API_ISSUER`, `APPLE_API_KEY` and `APPLE_API_KEY_PATH` from the child environment.
  This prevents credential-driven signing and notarization from stray exported values; it
  does not disable signing. The base config's `signingIdentity: "-"` intentionally lets
  Tauri apply the initial ad-hoc signature. Part 03 replaces it with the final Developer ID signature.
- **CI.** `quality.yml` gains an ad-hoc `bun run release` step after `tauri:build`. It uploads
  nothing and gains no secret. Add a `push.tags` trigger for `v*-rc.*` alongside the existing
  `main` push and pull-request triggers, so candidate tags on `feat/release` run every source
  gate on the exact tagged SHA. A nearby branch or pull-request SHA is not evidence for a candidate.
- **Script tests.** `scripts/release.test.ts` runs under the existing `test:scripts` Vitest
  project (`vitest.scripts.config.ts`, Node environment), which `bun run test` already
  includes. Add no runner and do not use `bun test`.

## Acceptance criteria

- [ ] `RELEASE-02-01` Name derivation yields `Stache_Next_0.26.0_aarch64.*` and
      `Stache_0.26.0_aarch64.*` from the two config shapes, with no hardcoded product. Test:
      `scripts/release.test.ts`.
- [ ] `RELEASE-02-02` Injected failures in the build, verifier, `hdiutil`, `ditto` and checksum
      steps each exit non-zero and leave no `SHA256SUMS` in the output directory. Test:
      `scripts/release.test.ts`, with fake command runners.
- [ ] `RELEASE-02-03` The verifier refuses a non-`arm64` executable, an extra file in
      `Contents/MacOS`, an unexpected file or a non-`arm64` Mach-O under `media-control`, a
      non-system linked library, a version mismatch, a missing `LSUIElement`, and a
      `CFBundleIdentifier` or `CFBundleName` that differs from the base config. Test:
      `scripts/release.test.ts`, with fake inspection output.
- [ ] `RELEASE-02-04` A second run replaces the output directory: `SHA256SUMS` has exactly two
      entries and stale artifacts are gone. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-02-05` Stray `APPLE_*` values are absent from the build child's environment.
      The initial config-driven ad-hoc signature remains intentional. Test: `scripts/release.test.ts`.
- [ ] `RELEASE-02-06` On a real checkout, `bun run release` produces all four outputs and
      `shasum -a 256 -c SHA256SUMS` passes. Check: the new `quality.yml` step; a workflow parse
      test proves candidate-tag triggering, and the candidate run records its exact SHA.
- [ ] `RELEASE-02-07` An ad-hoc `release.json` records `ad-hoc` and `notarized: false`, and
      the run's output does not state signed-by-Developer-ID, notarized or stapled. Test:
      `scripts/release.test.ts`; confirmed on the CI run.
- [ ] `RELEASE-02-08` The DMG mounts read-only with the app and `Applications` link, the app
      expanded from the zip passes `codesign --verify --strict --deep`, and its `Info.plist`
      matches the raw bundle. Check: scripted against a temporary mount and directory, in
      `bun run release`.
- [ ] `RELEASE-02-09` A drag from the mounted DMG installs and launches. Gated manual check on
      an approved run, recorded in Part 06, because it touches `/Applications`.

## Tests

Script tests inject the command runner, so they spawn no build and touch no real bundle. Real
packaging runs in `bun run release` and CI. Automated checks never write to `/Applications`.

## Notes

- `hdiutil`, `ditto`, `shasum`, `lipo`, `otool`, `plutil` and `codesign` ship with macOS and
  the Xcode command-line tools. Add no dependency.
- If strict deep verification of the Tauri-signed bundle fails because of the nested adapter,
  the script re-signs the nested Mach-O files and then the app ad hoc, in Part 03's order,
  before packaging. Record which happened in `release.json`; do not relax the check.
- Keep to one script plus what its tests require. There is no per-artifact configuration file.
- An ad-hoc artifact is a pipeline proof only. It is never uploaded as a release.
