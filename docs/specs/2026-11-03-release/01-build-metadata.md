---
status: draft
---

# 01 — Build metadata and one version

Read when changing the version, `Info.plist`, icons or packaged contents. **PLANNED.**
Depends on nothing else in this spec.

## Scope

Make the packaged bundle honest and reproducible: one authoritative version, metadata that
matches how the app runs, and a bundle containing only what it needs. The identity is not
renamed here; [Part 04](04-identity-switch.md) does that, and adds the dev-only overlay. Every
statement below concerns the base `tauri.conf.json`, the only config a package is built from.

## Requirements

- **One version.** `[workspace.package] version` in the root `Cargo.toml` is authoritative.
  Set it to `0.26.0`, and update `package.json`, `src/crates/app/tauri.conf.json` and the
  `Cargo.lock` workspace entries. The `version` in `src/ui/tests/hostFixtures.ts` is test
  data, outside the version check; change it only if a test needs it to.
- **Version check.** A Rust integration test, `src/crates/app/tests/release_metadata.rs`,
  parses the three manifests and fails on any difference, naming the file and both values.
  It runs in `bun run test`, so contributors need no extra command. Do not make
  `tauri.conf.json` derive its version at build time: this keeps the manifests readable and
  the mismatch visible.
- **`Info.plist`.** Add `src/crates/app/Info.plist` beside the Tauri config, where Tauri
  discovers and merges it without a config key. It sets `LSUIElement` true,
  `NSHighResolutionCapable` true and `LSApplicationCategoryType`
  `public.app-category.utilities`. Do not copy the legacy location strings: no shipped
  module requests location, and a future part adds a usage string with its permission.
- **Icons.** Keep the four referenced icons. `icons/icon.png` stays because `README.md`
  displays it. `icons/64x64.png` is referenced by nothing in the new tree; remove it, or
  reference it if a real use exists. Do not change the visual identity.
- **Bundle contents.** The expected `.app` is `Contents/MacOS/stache` as the only
  executable, `Contents/Info.plist`, `Contents/Resources/icon.icns` and `_CodeSignature`.
  Frontend assets are embedded in the binary by `custom-protocol`, so no separate frontend
  directory is expected. `Contents/Resources/media-control/` holds exactly the files and symlink
  targets in `scripts/media-control/manifest.json` (flattened by Tauri), plus `PROVENANCE.md` and
  `LICENSE`. Generator and smoke binaries and any other framework must be absent.
- **Linkage.** `Contents/MacOS/stache` must be arm64 only and link only system libraries
  under `/System` or `/usr/lib`. This rule covers `Contents/MacOS/stache` only; the nested
  `media-control` code is signed separately by Part 03.
- **Unchanged.** `bundle.targets` stays `["app"]`. The capability list stays exactly
  `test-window`. No entitlements file is added.

## Acceptance criteria

- [ ] `RELEASE-01-01` The three manifests declare `0.26.0`, and `release_metadata.rs` passes
      on them. Test: `release_metadata.rs`.
- [ ] `RELEASE-01-02` The version check fails on an injected mismatch in each of
      `package.json` and `tauri.conf.json`, and the message names the file. Test: parser unit
      cases in `release_metadata.rs`, using in-memory or temporary manifests.
- [ ] `RELEASE-01-03` The committed `Info.plist` holds exactly the three stated keys and
      values and no usage-description key. Test: `release_metadata.rs`.
- [ ] `RELEASE-01-04` Every icon in `bundle.icon` exists, every file in `icons/` is referenced
      by `bundle.icon` or is `icon.png`, and `icon.png` exists. Test: `release_metadata.rs`.
- [ ] `RELEASE-01-05` The capability list is exactly `["test-window"]`, `bundle.targets` is
      `["app"]`, and no `entitlements` key is set. Test: `release_metadata.rs`.
- [ ] `RELEASE-01-06` A built bundle matches the stated contents and linkage, and its merged
      `Info.plist` holds the three keys, `LSMinimumSystemVersion` `26.0`, and a
      `CFBundleIdentifier` and `CFBundleName` equal to the base config's `identifier` and
      `productName`. Check: the bundle verifier added in Part 02, run by `bun run release`.
- [ ] `RELEASE-01-07` A packaged app launched from Finder shows no Dock icon, creates its
      tray item, and opens the test window with its embedded document from the tray. Gated
      manual check, recorded in Part 06.

## Tests

`release_metadata.rs` covers the static criteria in the default suite and never builds a
bundle. The bundle checks need a real `.app`, so they run in the pipeline, not the fast suite.

## Notes

- `LSUIElement` hides the Dock icon from launch, before `ActivationPolicy::Accessory` runs
  in `src/crates/app/src/lib.rs`. Keep the runtime policy; the two do not conflict.
- `tauri-codegen` also embeds a neighboring `Info.plist` into the binary, so unbundled
  `bun run tauri:dev` runs read it too, with `CFBundleName` set from the merged `productName`.
  Confirm development behavior is unchanged.
- No built bundle was inspected for this spec; the contents and linkage above are
  expectations for `RELEASE-01-06` to prove. If they differ, fix the spec or the bundle and
  do not relax the check silently.
