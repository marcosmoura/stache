---
status: done
---

# 07 — Owned media adapter and artwork

Read when adding media after 06 and separate approval. Private MediaRemote compatibility is gated; absence hides media without taking down Bar.

## Scope

Add `modules/bar/src/providers/media/` parser/owner/artwork code and tests, app resource resolution/projection/bindings and fixed media actions, UI Media hooks and colocated tests. Reuse the landed `CommandExecutor` (`sdk/src/commands.rs`), which today has only one-shot captured runs (`run_captured`/`run_captured_prepared`, `:847,864`); add the minimal bounded streaming extension there, on the existing `ChildProcess` trait (`:487`), `RetainedChild` (`:646`) and `CommandExecutor` (`:710`) (bounded line reads, cancellation, exact-child kill/reap) with tests that keep captured callers unchanged, not a second subprocess service. Native/private calls remain in safe `stache-macos` wrappers where applicable. `BarView` has no media field yet, and `Media.tsx` (`src/ui/src/shell/windows/Bar/Media/`) returns null without a label. The UI icon/bundle map already exists (`Media.constants.ts` `MEDIA_APPS` `:18-35`, `PLAYER_ICONS` `:49-70`); the click map and host bundle map are new.

## Requirements

- Feasibility inputs, read for provenance only and never at runtime: `_legacy/app/native/resources/Frameworks/MediaRemoteAdapter.framework`, `_legacy/app/native/resources/lib/media-control/mediaremote-adapter.pl` and `MediaRemoteAdapterTestClient` next to it. The `tauri.conf.json` `bundle` (`:21-26`) has no `resources` yet; the packaging choice is made at the feasibility gate.
- Before production use, record macOS 26 feasibility of `/usr/bin/perl <mediaremote-adapter.pl> <MediaRemoteAdapter.framework> <MediaRemoteAdapterTestClient> stream --no-diff`, actual artifact provenance/hash/license/architecture, required resource paths and Next bundle identity. Debug and packaged resolution must not reach into `_legacy` at runtime or use a shared executable name. No unsupported promise that legacy bundled framework works on macOS 26. Compile/bundle inspection is not live feasibility; default checks spawn no adapter. Live stream/permissions/audio-player interactions need each-run approval.
- Start only accepted enabled/unpaused Bar. One exact owned child, bounded stdout line/total buffered bytes, stderr retention/rate, parse depth/field lengths and queue; UTF-8 fragmentation/multiline chunk handling before JSON framing. Parse wrapper `{ payload: ... }`, validate types and replace complete state, coalesce duplicate snapshots, sanitize diagnostics. Stop cancels reads/backoff, invalidates state first, terminates and reaps exact child/owned descendants if spawned by adapter; prove descendant ownership, never PID-name kill. Late data/exit/restart cannot affect successor. Restart crashes with bounded cancellation-wakeable backoff, not spin; repeated failure reports unavailable and hides Media while Bar stays Running.
- Published DTO is typed, versioned and generation/revision/sequence coherent through 03 bridge; no module locks/native work on read. Malformed/empty payload, child unavailable or no media renders legacy absent state. Preserve title + `-` + artist, `Paused: ` prefix and long-label scrolling exactly; no transport controls or new playback polling loop.
- Source facts: legacy media.rs accepts base64 `artworkData` plus MIME, rejects `<` textual placeholder, decodes header-limited PNG/JPEG/WebP/GIF/BMP, center-crops and Lanczos3 resizes to 128×128 PNG, stores **base64 text in `.txt`**, returns encoded `artwork` string; UI constructs `data:image/png;base64,` and preloads. It does not send a file URL. Preserve displayed image and loading/error behavior; do not promise byte-equal compressed PNG output across encoder upgrades. Current caps: encoded16MiB, decoded12MiB, axis8192, pixels16Mi. Preserve or tighten only with tested compatibility/no visual delta; bound entire stream frame before allocation as well as artwork decode. Cache/encoded read is bounded, atomic/private and Next-identity scoped, no traversal/symlink-following unsafe writes or reuse of legacy cache. Use content identity to prevent title/artist filename collisions; this internal correction must not change valid displayed artwork.
- Asset ownership survives cancellation until decode/native/child work actually cleans up. Ack/reap timeout reports cleanup-pending with retained exact-generation owner and bounded retries; do not equate kill issued with reaped. Host begin-shutdown wakes provider waits before joins; parsing/artwork cannot starve main or host. Empty/error artwork never adds new placeholder styling; test the source's actual preload/retained-art behavior rather than assuming it clears on every missing field.
- Exact fixed source/click/icon map from `_legacy/app/ui/renderer/bar/Media/Media.constants.ts`: Spotify `com.spotify.client` → SpotifyIcon green22; Microsoft Edge **Dev** `com.microsoft.edgemac.Dev` → YoutubeIcon red22; Tidal `com.tidal.desktop` → SiTidal text18; Feishin `org.jeffvli.feishin` → Vynil02Icon sky22. Unknown player uses PlayCircle02Icon text22, click inert. No stable Edge substitution, generic bundle launcher or icon swap. Host maps validated known identifier to fixed target, never trusts arbitrary player path/name. Extend 05 typed action outcomes and capability additively.

## Decisions

- Packaging (user, 2026-11-10): vendor the adapter artifacts (`mediaremote-adapter.pl`, `MediaRemoteAdapter.framework`, `MediaRemoteAdapterTestClient`) from `_legacy/app/native/resources/` into the app's own resources, with recorded upstream provenance, SHA-256 hashes, license and architecture, shipped via Tauri `bundle.resources`; dev resolves the same in-repo path. The runtime never reads `_legacy`. The macOS 26 feasibility run decides whether media is enabled.

- Playback stopped (user, 2026-11-10): an empty adapter payload hides Media (the legacy absent state). Legacy ignored empty payloads and kept the last track visible; this approved deviation replaces that.

- Title-less payload (user, 2026-11-10): a payload with an artist but no title hides Media; legacy rendered ' - Artist'. Approved deviation.

- Artwork CSP (user, 2026-11-10): `csp` and `devCsp` add `img-src 'self' data:` because the artwork is shown through `data:` URLs (legacy allowed `img-src 'self' data: blob:`). Tauri does not enforce the CSP under `tauri:dev`, so the live dev run could not catch the missing directive.

- macOS version (user, 2026-11-10): macOS 27 evidence counts for the macOS 26+ gates; macOS 26 itself is not separately verified. The app minimum stays macOS 26.0.

## Feasibility evidence (2026-11-10)

BAR-07-01 evidence, from the approved probe on macOS 27.0, arm64. By user decision (2026-11-10) macOS 27 evidence satisfies the macOS 26+ gate; macOS 26 itself was not separately verified. `/usr/bin/perl <abs>/mediaremote-adapter.pl <abs>/MediaRemoteAdapter.framework <abs>/MediaRemoteAdapterTestClient stream --no-diff` streams newline-delimited JSON `{"type":"data","diff":false,"payload":{...}}`. The first line has an empty payload; a track change emits one line without `artworkData` and then the same payload with base64 `artworkData` and `artworkMimeType` (largest observed line about 157 KB). Payload keys seen: playbackRate, album, mediaType, trackNumber, elapsedTime, timestamp, bundleIdentifier, processIdentifier, title, artist, artworkMimeType, artworkData, duration, contentItemIdentifier, playing, totalDiscCount, totalTrackCount. No prompt and no child processes (perl loads the framework in-process); SIGTERM ends it and it is reaped. Relative paths fail ("Failed to load framework"), so absolute paths are mandatory. The binaries are arm64 only. Provenance, hashes and license are in `src/crates/app/resources/media-control/PROVENANCE.md`; packaging and the Tauri symlink flattening are in [features/bar.md](../../features/bar.md#media-adapter-packaging). Slice A delivered packaging, resolution and the SDK line stream only.

## Acceptance criteria

- [x] BAR-07-01: Adapter packaging/provenance/macOS 26 feasibility gate is evidenced or explicitly unavailable; Bar never relies on legacy runtime resources/build or name-kill. (Evidenced on macOS 27.0, which counts for the macOS 26+ gate by user decision 2026-11-10.)
- [x] BAR-07-02: Bounded streaming/parser/artwork/cache and exact-generation child shutdown/reap/restart pass adversarial fake tests without blocking host/main.
- [x] BAR-07-03: Exact map/icons/labels/preload/hidden/paused states and fixed clicks round-trip typed bridge with independent visual parity and no baseline reset.

## Tests

Fake child/runner streams split UTF-8/JSON, oversized/never-terminated line, invalid wrapper/depth/types, stderr flood, duplicate/empty state, EOF/crash, backoff cancellation, blocked stdout/stderr/reap, stale old exit/completion and successor; assert owned child termination/reap exactly and zero real subprocess. Artwork fixtures: every MIME, malformed base64, encoded/decode/header/pixel caps, `<` placeholder, center crop and deterministic pixels, `.txt` encoded cache read limits, collision/path/symlink cases in temp Next cache. App fake resources missing/wrong architecture/identity, packaged path selection and authorization; no resources taken from legacy. WebKit all map/fallback icons/long strings/artwork success/error/stale-load/paused/absent plus typed launch uncertainty. Actual framework/stream/manual performance results remain gated, never inferred from fakes.

## Notes

Feature/testing/performance docs disclose private-API and hardware limits. No new prompt, player launch, audio mutation or legacy automation in default validation; final merge cannot claim gated media compatibility without evidence.

Completion record (2026-11-10):

- Slice A: adapter vendored in `src/crates/app/resources/media-control/` (provenance, BSD-3-Clause, SHA-256, arm64 only), shipped via `bundle.resources` (Tauri flattens the framework symlinks; the adapter loads only `<framework>/MediaRemoteAdapter`), typed resolver (bundle resources, debug-only in-repo fallback, never `_legacy`), SDK bounded line stream (per-line cap, stderr tail, pull backpressure, `poll(2)` idle wait with a 250 ms ceiling, exact-child SIGTERM → SIGKILL → reap).
- Slice B: generation-owned media provider (parse, coalesce, restart backoff 1/2/5/10/30/60 s, unavailable after 3 failures while Bar stays Running), artwork pipeline (caps before decode, center crop, Lanczos3 128×128 PNG, content-hash id) with a private Next-scoped cache, `bar_media_artwork` command (artwork never in `BarView`/`BarChanged`), fixed `BarAction::Media` map from the published bundle id.
- Slice C: UI mapping, artwork by id with legacy retained-image behavior, media click. Parity unchanged (962 cases).
- Approved deviations: an empty payload hides Media; a title-less payload with an artist hides Media (legacy rendered ' - Artist').
- Live run (user-observed, macOS 27.0, `bun run tauri:dev`, Spotify): title/artist and the `Paused:` prefix, artwork including the change on a new track, the click bringing up the player, and Media hiding when the player quits all work.
- Review fixes (2026-11-10): the CSP gains `img-src 'self' data:` in `csp` and `devCsp` for the artwork `data:` URLs (final CSP: `connect-src` with the three weather hosts plus `img-src 'self' data:`); the line stream's idle wait is `poll(2)` on the child pipes plus a cancel wake fd with no timeout, so an idle stream has zero wakeups (replacing the 250 ms ceiling of slice A); after 8 consecutive failures (5 more after it became `unavailable`; backoffs 1, 2, 5, 10, 30, 60, 60 s, 168 s in all) the adapter gives up (status `unavailable` with a "keeps failing; not restarted until Bar restarts" reason), and Media stays hidden until the next Bar start or resume (a plain config reload keeps the same run and does not restart it; use Pause then Resume); parity runs no longer rewrite the committed `run-record.{1x,2x}.json` (normal runs write to an untracked location under `test-results/`; the committed records update only with `STACHE_PARITY_WRITE_RECORD=1`).
- Live evidence ran on macOS 27.0; by user decision (2026-11-10) it satisfies the macOS 26+ gates. macOS 26 itself is not separately verified.
- Unverified until an approved live run: the Edge Dev/Tidal/Feishin clicks on real players.
- Release-app live run (2026-11-10, user-observed, `bun run tauri:build` bundle, macOS 27.0): media from the bundled adapter (flattened framework) with artwork under the production CSP works.
