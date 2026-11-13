---
branch: feat/release
---

# Release and packaging

Read when turning Stache Next from a development bundle into a distributable product.
**IN PROGRESS ON `feat/release`, NOT SHIPPED.** Parts 01 and 02 are implemented on the branch;
Parts 03, 04 and 05 are implemented up to their gates; Part 06 is not started. Nothing below is
merged, and no artifact has been signed, notarized or published. Written against `ad34b46`;
revised 2026-11-13 against `main` at `74bfd27` (see [Revision](#revision-2026-11-13)).

## Goal

Finish the app as an installable macOS product: one authoritative version and honest
bundle metadata, a repeatable release pipeline, Developer ID signing and notarization,
the final `Stache` identity, install/upgrade/removal rules that never fight the legacy
app, and release gates that state only what was proven.

## Non-goals

- No runtime features, new settings surface, new tray items or module behavior. Keep Awake,
  Wallpapers, Keybindings, Hold-to-Quit, MenuAnywhere, Bar and Tiling keep their own specs.
- No auto-updater and no launch-at-login item. The legacy app had neither.
- No automatic configuration migration. `stache config migrate` stays the only path that
  converts a legacy file; a release may detect one and print a diagnostic.
- No Homebrew Core submission, App Store distribution or universal/Intel binary.
- No silent removal or termination of the legacy application.
- No further teardown work. [Coordinated application teardown](../2026-11-03-teardown/00-overview.md)
  shipped: tray Quit, real Cmd-Q and SIGTERM are verified. Logout, restart and shutdown are
  not verified, and this spec publishes that as a limitation.

## Shipped state today

Verified on disk at the base commit.

- `src/crates/app/tauri.conf.json`: `productName` `Stache Next`, identifier
  `com.marcosmoura.stache.next`, version `0.25.0`, `bundle.targets` `["app"]`, ad-hoc
  `signingIdentity` `"-"`, `minimumSystemVersion` `26.0`, `entitlements`
  `Entitlements.plist`, no fixed windows and three capabilities (`access-window`,
  `bar-window`, `popover-window`). Four icons are referenced; `icons/64x64.png` and
  `icons/icon.png` are not, and `README.md` displays `icon.png`.
- `bundle.resources` maps `resources/media-control/` into
  `Contents/Resources/media-control/`: the Bar's vendored MediaRemote adapter, with two
  Mach-O files (`MediaRemoteAdapterTestClient` and the `MediaRemoteAdapter.framework`
  library), a perl script and `PROVENANCE.md`. Tauri flattens the framework's symlinks.
  Upstream's build signs them ad hoc. `fix/security-hardening` re-vendors these files;
  that branch is independent of this one.
- The version is also in `package.json` and `[workspace.package]`, and `Cargo.lock` repeats
  it. Nothing keeps these in sync. `src/ui/tests/hostFixtures.ts` also holds `0.25.0`, as
  fixture payload rather than a manifest, so it stays outside the version check.
- `src/crates/app/Info.plist` holds the two location usage strings the Bar's weather needs,
  and `Entitlements.plist` holds only `com.apple.security.personal-information.location`.
  No `LSUIElement`, DMG target or notarization configuration exists. The tray-only
  behavior comes from `ActivationPolicy::Accessory` at runtime.
- `.github/workflows/quality.yml` runs generated checks, lint, format, tests, coverage,
  audits, `bun run build` and `bun run tauri:build` on `macos-26`. `bun run test` already
  includes `test:scripts`, a Node-environment Vitest project (`vitest.scripts.config.ts`)
  that collects `scripts/**/*.test.ts`. It uses no signing
  secret and uploads no artifact. Its triggers are pushes to `main` and pull requests only;
  candidate tags on `feat/release` need Part 02's added candidate-tag trigger.
- `build.rs` fails a release build without `custom-protocol`, which `bun run tauri:build`
  passes. Generator and smoke binaries declare `required-features`. That they stay out of
  the bundle is documented, but no check asserts it; Part 01 adds one.
- A bundled executable always selects desktop mode (`stache_cli::mode::is_app_bundle`), so
  `/Applications/<App>.app/Contents/MacOS/stache config migrate` starts the desktop rather
  than running the command. Part 05 addresses this.
- Cache and runtime directories derive from the compile-time constant
  `stache_sdk::paths::IDENTITY`, which no Tauri config reaches. Configuration resolves to
  `$XDG_CONFIG_HOME/stache` or `~/.config/stache`, the directory that also holds the legacy
  `config.jsonc`. Stache Next reads `global.toml` and per-module `<id>.toml` files there.
- The Tauri CLI (2.12.4) merges repeated `--config` arguments in order and passes the result
  to the build through `TAURI_CONFIG`. `PackageInfo.name` is compiled from the merged
  `productName`. An unbundled dev binary embeds an `Info.plist` with `CFBundleName` and the
  version but no `CFBundleIdentifier`.
- No Developer ID identity exists on the development machine: its keychain lists only
  `Stache App` and `Apple Development`. `marcosmoura/stache` has no release or tag.
  `marcosmoura/homebrew-tap` is public with a `Formula` directory and no `Casks` directory.

Legacy `Stache` (`com.marcosmoura.stache`, macOS 14 minimum) is built by
`_legacy/scripts/release.sh`, which signs with a self-signed local `Stache App`
certificate, copies the bundle to `/Applications` and runs `cargo install` for a `stache`
binary. Its `Info.plist` sets `LSUIElement`, `NSHighResolutionCapable`,
`LSApplicationCategoryType` and two location strings. It bundles `resources/lib` and
`resources/Frameworks`, and its README warns that the retained
`MediaRemoteAdapter.framework` fails strict deep-signature validation. It has no updater
and no login item. Its cache **and control socket** live under
`~/Library/Caches/com.marcosmoura.stache`. This tree bundles only the `media-control`
adapter described above.

## Decisions

1. **Final identity is `Stache` / `com.marcosmoura.stache`, switched in stages.** Release
   builds take it at the switch; development keeps `com.marcosmoura.stache.next` and the
   `Stache Next` name permanently (Decision 12). The switch is
   [Part 04](04-identity-switch.md), gated on the legacy app being removed and approved for
   removal. No process is ever identified or terminated by the shared executable name
   `stache`.
2. **arm64 only, macOS 26 or later.** Published as a limitation.
3. **DMG plus zip, Developer ID signed, notarized and stapled, published to GitHub
   Releases, with a cask in `marcosmoura/homebrew-tap`.**
4. **No Developer ID certificate or notarytool credentials exist yet.** See
   [Blocked gates and merge consequence](#blocked-gates-and-merge-consequence).
5. **First release version is `0.26.0`.** `[workspace.package] version` is authoritative;
   a check keeps `package.json` and `tauri.conf.json` identical to it.
6. **No auto-updater, launch-at-login or automatic config migration.** Settings are the
   shipped tray popover editor and the TOML files; this spec adds no settings surface.
7. **Coexistence is allowed for now.** Legacy Stache and Stache Next may both be installed.
   Once the migration is approved, removing the legacy app is an explicit documented user
   step, never automatic and never by process name.
8. **No runtime features in this spec.**

Decisions added by this spec, each resting on a verified fact above:

9. **The cask stops the app with `signal: [["TERM", <bundle id>]]` and `on_upgrade: :signal`,
   not `quit:`.** Both routes now reach the coordinated teardown, but `quit:` sends an Apple
   Event and so needs Automation permission for the terminal, while SIGTERM needs none.
   Homebrew matches `signal:` by bundle identifier, not executable name.
10. **The DMG is built with `hdiutil` from the finished app.** Tauri's `dmg` target stays
    unused so one tool owns the layout and stapling order, and `bundle.targets` stays `["app"]`.
11. **The first public artifact carries the final identity.** The merge gate puts Part 04
    before any tag on `main`, so no `Stache Next` release and no `.next` cask are planned.

Decisions from the owner's answers:

12. **Development keeps `.next` through a committed, dev-only overlay.** `bun run tauri:dev`
    merges `src/crates/app/tauri.dev.conf.json` over the base config with a second `--config`;
    `bun run tauri:build` uses the base config alone. A test asserts the packaged identifier
    is the final one. The display name follows the merged `productName` at runtime and falls
    back to a product constant where no Tauri context exists. See
    [Part 04](04-identity-switch.md#development-identity).
13. **Released CLI access is a symlink, with a pre-approved fallback.** If the symlink route
    fails, a small separate CLI binary ships outside the app bundle, without a fresh decision.
    Mode selection does not change. See [Part 05](05-install-upgrade-uninstall.md#cli-access).
14. **A running release and a development run are siblings.** Collision detection refuses only
    a foreign process sharing this process's exact identifier, and warns for a detectable sibling
    bundle. Unbundled dev runs have no bundle identifier: require coexistence and path isolation,
    not a warning when the release cannot detect them.
15. **`IDENTITY` is selected at compile time by build profile.** Debug builds resolve
    `stache_sdk::paths::IDENTITY` to `com.marcosmoura.stache.next` and release builds to
    `com.marcosmoura.stache`. The release profile keeps debug assertions off, and a test refuses a
    release profile that enables them. No environment variable, config key or CLI flag selects the
    identity. The overlay cannot reach this constant, and without this rule a `tauri:dev` run would
    share the installed release's lock, socket and cache. See
    [Part 04](04-identity-switch.md#development-identity).

Decisions from the 2026-11-13 revision:

16. **Nested code is signed individually, inside out.** Every Mach-O under
    `Contents/Resources/media-control/` is signed before the main executable, which is
    signed before the bundle. `--deep` is never used to sign. The bundle verifier accepts
    exactly the vendored files there and nothing else.
17. **The committed entitlements stay, and stay minimal.** `Entitlements.plist` holds only
    the location entitlement the Bar's weather needs under hardened runtime. The final
    signature applies it to the main executable only, never to the nested adapter.
18. **Both gates are still closed on 2026-11-13.** The owner confirmed that no Developer ID
    credentials exist and legacy removal is not approved. This session implements Part 01,
    Part 02, Part 03 up to its gate, Part 04 Stage 1 and Part 05's preparation, then keeps
    `feat/release` unmerged. Parts 03, 04 and 05 stay `in-progress` and Part 06 `draft`.
19. **`feat/release` starts from `main`, independent of `fix/security-hardening`.**
    Whichever merges second resolves the overlap in `quality.yml` and this spec.
20. **Legacy removal was approved and performed on 2026-11-13**, superseding the Part 04 half of
    Decision 18; see the [Stage 2 record](04-identity-switch.md#stage-2-record). Stage 3 follows.
    Part 03 stays blocked on credentials.
21. **The owner installs an ad-hoc build of the renamed app in `/Applications` for their own use**
    before credentials exist. This is a local owner install from `bun run release`, done by hand
    at the owner's request; it is not a release, is not distributable, and proves none of the
    signed-artifact criteria. Ad-hoc signatures change with every build, so macOS privacy grants
    such as Accessibility must be given again after each reinstall.

## Blocked gates and merge consequence

Two parts cannot finish on the strength of code alone:

- **[Part 03](03-signing-notarization.md) is blocked on credentials.** Its pipeline and
  verification may be written and dry-run ad-hoc, but it cannot become `done` until a
  Developer ID Application certificate and notarytool credentials exist.
- **[Part 04](04-identity-switch.md) is gated on legacy removal.** Its preparation may
  land, but the rename waits for recorded approval and the removal itself. The owner's checkout-based
  migration rehearsal and removal precede the rename; the public removal/install/released-CLI
  migration sequence is separate and is rehearsed with private candidate bytes before merge.

**Part 06 completion covers private candidate evidence only.** Exact-SHA source gates require
the added candidate-tag CI trigger. Authenticated draft downloads support artifact checks and
local test-cask rehearsals with distinct test-only versions, not online audit or public tap checks.
Those public URL checks remain explicitly not run until final promotion, in a separate
[post-merge operational checklist](06-release-verification.md#post-merge-operational-checklist),
not in any part's completion criteria. No public prerelease is introduced. Part 05 completes only
after the selected CLI route passes; its mutually exclusive alternative is recorded not applicable,
never passed. These stage and applicability boundaries do not waive any required private check.

The [merge gate](../../workflow.md#merge-gate) refuses a branch while any part is not
`done`. **`feat/release` therefore cannot merge until both gates clear and Part 06 has
verified the result.** Keeping the branch stays allowed, and rewrite branches continue
from `main` in the meantime. A credential or approval is recorded evidence of a gate, never
a claim made by this spec.

## Parts

| Part                                  | Summary                                                                   | Status      |
| ------------------------------------- | ------------------------------------------------------------------------- | ----------- |
| [01](01-build-metadata.md)            | One version, `Info.plist`, icon rule, bundle-content check.               | done        |
| [02](02-release-pipeline.md)          | Artifacts, ad-hoc dry run, exact-SHA candidate Quality trigger.           | done        |
| [03](03-signing-notarization.md)      | Final Developer ID re-sign, notarization, stapling, CI gate. **Blocked.** | in-progress |
| [04](04-identity-switch.md)           | Staged switch, dev isolation, detectable sibling warnings. **Gated.**     | in-progress |
| [05](05-install-upgrade-uninstall.md) | Executable migration/removal, exclusive CLI routes, local test casks.     | in-progress |
| [06](06-release-verification.md)      | Private candidate gates; separate public operational checklist.           | draft       |

Implementation order differs from numbering at one point, because the rename needs the
removal steps to exist first: 01 → 02 → 03 (to its gate) → 04 preparation → 05 → 04 rename
→ 03 credentials and 06. Part 02 depends on 01, Part 03 on 02 (including candidate source
triggering), Part 05's preparation on 02 and 04 preparation, and its shipped-route proof on
03 signing and 04 rename. Part 06 integrates all of them, including Part 05's full private
migration and local-cask upgrade rehearsal; public URL operations follow merge and promotion,
not part completion. Each commit must build and pass `bun run lint`,
`bun run format:check` and `bun run test`.

## Resolved questions

Answered by the owner.

1. **Must the legacy app be removed before the first public release?** Yes. There is no public
   release before the rename: the first public artifact carries the final identity (Decision 11).
2. **Does development become `Stache` after the rename?** No. Release builds do; development
   keeps `.next` permanently (Decision 12).
3. **Is a `binary` symlink an acceptable CLI for released installs?** Yes, with a pre-approved
   fallback (Decision 13). `RELEASE-05-07` decides between them; neither route needs a fresh
   decision.
4. **How does the runtime identity follow the build?** By build profile, at compile time only
   (Decision 15). A Cargo feature passed only by `tauri:dev` would leave a plain `cargo run` CLI on
   the final identity, unable to reach a dev host, and a release binary should not honor a runtime
   override.

No open questions remain.

## Revision 2026-11-13

The tree moved after this spec was written. Changed to match `main`: the bundle carries the
nested `media-control` adapter (Decision 16); `Info.plist` and `Entitlements.plist` exist
and keep their location entries (Decision 17); the capabilities are the three shipped
windows; the Test Window is gone, so manual checks open the tray popover; script tests use
the existing `test:scripts` Vitest project instead of `bun test`; teardown shipped, so
Cmd-Q is no longer a published gap; and the popover settings editor shipped, so "no
Settings UI" is no longer a limitation. Decision 18 records how far this session goes.

## Affected docs

Now: [index.md](../../index.md) and [roadmap.md](../../roadmap.md) gain additive entries.
At implementation: [README.md](../../../README.md), new `docs/guides/install.md` and
`docs/guides/release.md`, [testing.md](../../guides/testing.md), [overview.md](../../architecture/overview.md#identity),
[ipc.md](../../architecture/ipc.md#identity-paths),
[performance.md](../../guides/performance.md) and `AGENTS.md`, each in the part that makes
its statement true. Archived specs and `_legacy/` stay as they are.
