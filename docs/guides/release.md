# Release pipeline

Read when building a distributable artifact, running `bun run release` or changing `scripts/release.ts`.

Only the ad-hoc pipeline ships. Developer ID signing, notarization, stapling and publishing
are **not shipped** (planned in [Part 03](../specs/2026-11-03-release/03-signing-notarization.md)).
An ad-hoc artifact is a pipeline proof only and is never published.

## Command

`bun run release` runs `scripts/release.ts` (logic in `scripts/release/pipeline.ts`):

1. Records the commit and whether the tree is clean, then runs `bun run tauri:build` with
   `APPLE_SIGNING_IDENTITY`, `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`,
   `APPLE_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_API_ISSUER`, `APPLE_API_KEY` and
   `APPLE_API_KEY_PATH` removed from the child environment. The base config's
   `signingIdentity: "-"` still applies Tauri's ad-hoc signature.
2. Verifies `target/release/bundle/macos/<productName>.app`: `Contents/MacOS` holds only
   `stache`; `lipo -archs` is exactly `arm64` on it and on every nested Mach-O under
   `Contents/Resources/media-control/`; the executable links only under `/System` or `/usr/lib`;
   `media-control` holds exactly the vendored files of `src/crates/app/resources/media-control/`
   as Tauri flattens them (file links become files, directory links such as
   `Versions/Current` are dropped); no framework or generator/smoke binary elsewhere; the merged
   `Info.plist` has `LSUIElement`, `NSHighResolutionCapable`, `LSApplicationCategoryType`,
   `LSMinimumSystemVersion` `26.0`, `CFBundleShortVersionString` equal to the root `Cargo.toml`
   `[workspace.package]` version and `CFBundleIdentifier`/`CFBundleName` equal to the base
   config.
3. Copies the app into a staging directory and runs `codesign --verify --strict --deep`. If that
   fails, it re-signs ad hoc inside out (nested Mach-O, main executable, bundle; never `--deep`;
   entitlements only on the executable and bundle) and records `adHocResigned: true`.
4. Builds the DMG with `hdiutil` (UDZO, volume name `<productName>`, the app plus an
   `Applications` link) and the zip with `ditto -c -k --keepParent`.
5. Mounts the DMG read-only at a temporary mount point (always detached), checks the app and
   link, expands the zip into a temporary directory, verifies its signature strictly and compares
   its `Info.plist` with the raw bundle. Nothing writes to `/Applications`.
6. Writes `release.json` and `SHA256SUMS`, then moves the staging directory to
   `target/release-artifacts/`.

The output directory is removed at the start of each run, so a failed run leaves no
`SHA256SUMS`, and a successful run replaces everything.

## Outputs

In `target/release-artifacts/` (ignored with `target/`), `<Product>` being `productName` with
spaces as underscores and the version that of `tauri.conf.json`:

- `<Product>_<version>_aarch64.dmg` and `<Product>_<version>_aarch64.zip`; the app inside is
  always `<productName>.app`,
- `SHA256SUMS` with bare file names: `shasum -a 256 -c SHA256SUMS` from that directory,
- `release.json`: `version`, `commit`, `treeClean`, `signing: "ad-hoc"`, `notarized: false`,
  `adHocResigned`, `artifacts`.

## CI

`quality.yml` also runs on `v*-rc.*` tags and, after `tauri:build`, runs `bun run release` and
`shasum -a 256 -c SHA256SUMS`. It uploads nothing and uses no secret.
