# Release pipeline

Read when building a distributable artifact, running `bun run release`, changing `scripts/release.ts` or `.github/workflows/release.yml`.

The ad-hoc pipeline and the ad-hoc signing dry run ship. The Developer ID mode (`--sign`) and
`release.yml` are written and unit-tested with fake runners but have **never produced a signed or
notarized artifact**: no Developer ID certificate or notarytool credentials exist yet
([Part 03](../specs/2026-11-03-release/03-signing-notarization.md) gate). An ad-hoc artifact is
a pipeline proof only and is never published.

## Modes

| Command                             | Signature                                                 | Notarization         | `release.json`                                 |
| ----------------------------------- | --------------------------------------------------------- | -------------------- | ---------------------------------------------- |
| `bun run release`                   | Tauri ad hoc, re-signed only if strict verification fails | none                 | `ad-hoc`, `notarized: false`                   |
| `bun run release --dry-run-signing` | identity `-`, hardened runtime, `--timestamp=none`        | skipped (logged)     | `ad-hoc`, `notarized: false`, `mode`           |
| `bun run release --sign`            | Developer ID, hardened runtime, secure timestamp          | `notarytool`, staple | `developer-id`, `notarized: true`, submissions |

Any other argument is an error.

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
   `Versions/Current` are dropped); outside it, only `Contents/Info.plist`, the executable,
   `Contents/Resources/icon.icns` (required) and `Contents/_CodeSignature/CodeResources`; no
   framework or generator/smoke binary elsewhere; the merged
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
6. Writes `SHA256SUMS`, renders `Casks/stache.rb`, then writes `release.json`, then moves the
   staging directory to `target/release-artifacts/`.

The output directory is removed at the start of each run, so a failed run leaves no
`SHA256SUMS`, and a successful run replaces everything. Stale `target/release-artifacts.staging-*`
directories from an interrupted run are removed first, after a best-effort detach of any image
still mounted under them.

## Signing modes (`--dry-run-signing`, `--sign`)

After the bundle verifier and the copy into staging, both modes run the same order:

1. Sign inside out with `codesign --force --sign <identity> --options runtime <timestamp>`:
   each nested Mach-O under `media-control` as a plain file, then `Contents/MacOS/stache`, then
   the bundle. Only the executable and bundle get `--entitlements` (the committed
   `Entitlements.plist`). `--deep` is never used to sign. Then `codesign --verify --strict --deep`
   and `codesign -dvvv`.
2. Zip the app with `ditto` and `xcrun notarytool submit --wait --output-format json`; anything
   but `Accepted` fails.
3. `xcrun stapler staple` and `validate` the app.
4. Build the DMG from the stapled app, `codesign` it, notarize, staple and validate it.
5. Build the distributable zip from the stapled app.
6. Final checks on the artifacts, then `SHA256SUMS`, then `release.json`.

Final checks: strict deep verification and `codesign -dvvv` (hardened-runtime flag; in `--sign`
also `Authority=<identity>`, `TeamIdentifier=<team>` and `Timestamp=`) on the app, the app in the
mounted DMG and the app in the expanded zip; in `--sign` also `spctl --assess --type execute -vv`
on the app, `spctl --assess --type open --context context:primary-signature -vv` on the DMG and
`xcrun stapler validate` on the DMG and both contained apps.

`--dry-run-signing` uses identity `-` and `--timestamp=none`; notarization, stapling, `spctl`
and the authority/team/timestamp checks are logged as **skipped**, never as passed. It proves
the inside-out order, that the bundle still verifies strictly under hardened runtime and that
the runtime flag lands. It does not prove Developer ID acceptance, notarization, stapling or
Gatekeeper.

### `--sign` inputs

Read from the environment; missing or wrong input is an error before any build, never a fall
back to ad hoc:

- `STACHE_SIGNING_IDENTITY`: exactly `Developer ID Application: <name> (<team>)`. `Stache App`,
  `Apple Development: …` and `-` are refused.
- `STACHE_TEAM_ID`: the ten-character team identifier; it must match the identity's.
- Exactly one notarytool credential: the App Store Connect API key (`STACHE_NOTARY_KEY_ID`,
  `STACHE_NOTARY_ISSUER`, `STACHE_NOTARY_KEY_PATH` to the `.p8`; CI) or
  `STACHE_NOTARY_KEYCHAIN_PROFILE`, a profile created locally with
  `xcrun notarytool store-credentials` (Apple ID and app-specific password stay in the keychain).

The tree must be clean. The names are deliberately not `APPLE_*`: those are what Tauri reads and
what the build child has scrubbed, so the script's inputs never reach Tauri and the scrub keeps
its single meaning. Credentials go only to `notarytool` and are never logged or written to
`release.json`.

On rejection the run fetches `xcrun notarytool log <id>` into
`target/release-evidence/notary-<app|dmg>-<id>.json` (cleared at the start of each run, outside
the artifact directory) and fails. A failure at any step leaves no `SHA256SUMS` and no
`release.json`, so nothing records `notarized: true` unless every step succeeded.

## Outputs

In `target/release-artifacts/` (ignored with `target/`), `<Product>` being `productName` with
spaces as underscores and the version that of `tauri.conf.json`:

- `<Product>_<version>_aarch64.dmg` and `<Product>_<version>_aarch64.zip`; the app inside is
  always `<productName>.app`,
- `SHA256SUMS` with bare file names: `shasum -a 256 -c SHA256SUMS` from that directory,
- `release.json`: `version`, `commit`, `treeClean`, `signing` (`"ad-hoc"` or
  `"developer-id"`), `notarized`, `adHocResigned`, `artifacts`; the signing modes add `mode`, and
  `--sign` adds `notarySubmissions` (`app`, `dmg`).
- `Casks/stache.rb`: the Homebrew cask (`scripts/release/cask.ts`, `renderCask`), from the
  authoritative version and the DMG's SHA-256. It names the DMG on the `v<version>` GitHub
  release, `depends_on arch: :arm64` and macOS 26 (`">= :tahoe"`, one constant),
  `app "<productName>.app"`, a `binary` link to `Contents/MacOS/stache` (the CLI route, see
  [install](install.md#cli-access)), `uninstall signal: [["TERM", "<identifier>"]],
  on_upgrade: :signal` and `zap` of `~/Library/Caches/<identifier>` only. It is not an artifact:
  `SHA256SUMS` and `release.json` list only the DMG and zip. Publishing it to the tap is manual
  (release Part 06); nothing pushes it.

### Local test cask

For private candidate rehearsals only, `bun scripts/release/cask.ts --test-version 0.26.0-test.1
--url http://127.0.0.1:<port>/<file>.dmg --dmg <candidate DMG> [--out <file>]` renders a cask
marked `LOCAL TEST CASK` with that test version, the loopback URL and the candidate DMG's real
checksum. It refuses a non-loopback URL or a version not shaped `x.y.z-test.n`. It is separate
from `bun run release`: the production rendering takes no override, and the test version never
reaches the app, `release.json`, the manifests or `Casks/stache.rb`. Never publish it.

## CI

`quality.yml` also runs on `v*-rc.*` tags and, after `tauri:build`, runs `bun run release`, then
`bun run release --dry-run-signing`, each followed by `shasum -a 256 -c SHA256SUMS`. Both
release runs repeat `tauri:build`, which is incremental after the first build; the script does
not reuse a prebuilt bundle, so the bundle it signs is always the one it just verified. It
uploads nothing and uses no secret.

`release.yml` runs only on `v*` tag pushes, on `macos-26`, in the protected `release` GitHub
environment:

1. `scripts/release/gate.ts` fails unless the tag is `v<version>` or `v<version>-rc.<n>` with
   the `[workspace.package]` version, and unless the newest Quality run for the exact tagged SHA
   (`gh run list --workflow quality.yml --commit`) completed with `success`. Missing, failed or
   other-SHA runs block; a pending run is polled once a minute for up to 60 minutes.
2. The signing step alone receives the secrets (`DEVELOPER_ID_P12_BASE64`,
   `DEVELOPER_ID_P12_PASSWORD`, `NOTARY_API_KEY_P8`, `STACHE_SIGNING_IDENTITY`, `STACHE_TEAM_ID`,
   `STACHE_NOTARY_KEY_ID`, `STACHE_NOTARY_ISSUER`), imports the certificate into a temporary
   keychain, writes the API key to a file and runs `bun run release --sign`.
3. An `if: always()` step deletes the keychain and key file.
4. After `shasum -a 256 -c SHA256SUMS`, `gh release create --draft` uploads the DMG, zip,
   `SHA256SUMS` and `release.json` (`-rc.` tags as prereleases). Any failure deletes a draft
   that a partial upload left behind. Promotion is manual.
