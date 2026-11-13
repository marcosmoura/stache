# Release pipeline

Read when building a distributable artifact, running `bun run release`, or changing `scripts/release.ts`.

**No release has been published yet.** Releases are signed with the owner's self-signed
certificate and are **never notarized**: there is no Developer ID. An ad-hoc artifact is a
pipeline proof only and is never published.

## Modes

| Command                                                            | Signature                                                 | `release.json`                                         |
| ------------------------------------------------------------------ | --------------------------------------------------------- | ------------------------------------------------------ |
| `bun run release`                                                  | Tauri ad hoc, re-signed only if strict verification fails | `ad-hoc`, `notarized: false`                           |
| `STACHE_SIGNING_IDENTITY="Stache App" bun run release --self-sign` | the named self-signed certificate, hardened runtime       | `self-signed`, `notarized: false`, `certificateSha256` |

`Stache App` is the owner's certificate name; the script never hardcodes one. Any other argument
is an error. CI runs only the ad-hoc mode.

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

## Self-signed mode (`--self-sign`)

### Why sign

macOS ties privacy grants such as Accessibility to the code signature. An ad-hoc signature
differs on every build, so every update asks for the grant again. A signature from one certificate
has a designated requirement anchored on that certificate (`certificate root`, or `certificate leaf` for one with an issuer), so the grant
survives updates signed with it.

### What it does not do

The build is not notarized and nothing contacts Apple (no secure timestamp, `notarytool`,
`stapler` or `spctl`). Gatekeeper still blocks the first open of a downloaded copy until the
person allows it; the [install guide](install.md) covers that step.

### Steps

Before any build, the run refuses, with no fallback to ad hoc, when `STACHE_SIGNING_IDENTITY` is
missing, empty or `-`, or when `security find-identity -p codesigning` does not list that exact
name between quotes. It then reads the certificate's SHA-256 fingerprint from
`security find-certificate -a -c <identity> -Z` and refuses unless exactly one certificate
matches. After the build (whose child also loses `STACHE_SIGNING_IDENTITY`, `GH_TOKEN` and
`GITHUB_TOKEN`) and the bundle verifier:

1. Sign the staged copy inside out with
   `codesign --force --options runtime --timestamp=none --sign <identity>`: each nested Mach-O
   under `media-control` as a plain file, then `Contents/MacOS/stache`, then the bundle. Only the
   executable and bundle get `--entitlements` (the committed `Entitlements.plist`). `--deep` is
   never used to sign. The ad-hoc re-sign fallback uses the same helper with identity `-` and no
   hardened runtime.
2. Build the DMG and the zip from the signed app; sign the DMG with
   `codesign --force --timestamp=none --sign <identity>`.
3. On the staged app, the app in the mounted DMG and the app expanded from the zip:
   `codesign --verify --strict --deep`; `codesign -dvvv` shows `Authority=<identity>` and the
   hardened-runtime flag; `codesign -d -r-` shows a designated requirement containing
   `certificate root` or `certificate leaf`, and no `cdhash`.

Any failure exits non-zero and leaves no `SHA256SUMS` or `release.json`. macOS may ask once to let
`codesign` use the key.

### Creating a certificate

Someone without a certificate creates one in Keychain Access: Keychain Access → Certificate
Assistant → Create a Certificate…, give it a name (for example `Stache App`), Identity Type
**Self Signed Root**, Certificate Type **Code Signing**, then Create. Pass that name in
`STACHE_SIGNING_IDENTITY`. The private key is the only thing that ties a release to its signer:
it stays in the login keychain and never goes to CI. If it is lost, the next release has a new
signature and every user grants Accessibility once more.

## Outputs

In `target/release-artifacts/` (ignored with `target/`), `<Product>` being `productName` with
spaces as underscores and the version that of `tauri.conf.json`:

- `<Product>_<version>_aarch64.dmg` and `<Product>_<version>_aarch64.zip`; the app inside is
  always `<productName>.app`. The base config gives `Stache_<version>_aarch64.dmg`,
  `Stache_<version>_aarch64.zip` and `Stache.app` (`com.marcosmoura.stache`); the development
  overlay `tauri.dev.conf.json` is used only by `tauri:dev` and never reaches a release,
- `SHA256SUMS` with bare file names: `shasum -a 256 -c SHA256SUMS` from that directory,
- `release.json`: `version`, `commit`, `treeClean`, `signing` (`"ad-hoc"` or `"self-signed"`),
  `notarized` (always `false`), `adHocResigned`, `artifacts`; `--self-sign` adds
  `certificateSha256`, the public SHA-256 fingerprint of the signing certificate, so a person can
  compare releases.
- `Casks/stache.rb`: the Homebrew cask (`scripts/release/cask.ts`, `renderCask`), from the
  authoritative version and the DMG's SHA-256. It names the DMG on the `v<version>` GitHub
  release, `depends_on arch: :arm64` and macOS 26 (`">= :tahoe"`, one constant),
  `app "<productName>.app"`, a `binary` link to `Contents/MacOS/stache` (the CLI route, see
  [install](install.md#cli-access)), `uninstall signal: [["TERM", "<identifier>"]]` with
  `on_upgrade: :signal` (Homebrew sends SIGTERM to the app with that bundle identifier, never by
  process name; the cask deliberately does not use `quit:`), and `zap` of
  `~/Library/Caches/<identifier>` only. It is not an artifact: `SHA256SUMS` and `release.json`
  list only the DMG and zip. Publishing it to the tap is manual
  (release Part 06); nothing pushes it.

### Local test cask

For private candidate rehearsals only, `bun scripts/release/cask.ts --test-version 0.26.0-test.1
--url http://127.0.0.1:<port>/<file>.dmg --dmg <candidate DMG> [--out <file>]` renders a cask
marked `LOCAL TEST CASK` with that test version, the loopback URL and the candidate DMG's real
checksum. It refuses a non-loopback URL or a version not shaped `x.y.z-test.n`. It is separate
from `bun run release`: the production rendering takes no override, and the test version never
reaches the app, `release.json`, the manifests or `Casks/stache.rb`. Never publish it.

## CI

`quality.yml` also runs on `v*-rc.*` tags and, after `tauri:build`, runs `bun run release`
followed by `shasum -a 256 -c SHA256SUMS`. The release run repeats `tauri:build`, which is
incremental after the first build; the script does not reuse a prebuilt bundle, so the bundle it
packages is always the one it just verified. It uploads nothing, uses no secret and never signs
with a certificate.
