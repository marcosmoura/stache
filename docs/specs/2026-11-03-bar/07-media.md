---
status: draft
---

# 07 — Owned media adapter and artwork

Read when adding media after 06 and separate approval. Private MediaRemote compatibility is gated; absence hides media without taking down Bar.

## Scope

Add `modules/bar/src/providers/media/` parser/owner/artwork code and tests, app resource resolution/projection/bindings and fixed media actions, UI Media hooks and colocated tests. Reuse the landed `CommandExecutor` (`sdk/src/commands.rs`), which today has only one-shot captured runs (`run_captured`/`run_captured_prepared`, `:847,864`); add the minimal bounded streaming extension there (bounded line reads, cancellation, exact-child kill/reap) with tests that keep captured callers unchanged, not a second subprocess service. Native/private calls remain in safe `stache-macos` wrappers where applicable.

## Requirements

- Before production use, record macOS 26 feasibility of `/usr/bin/perl <mediaremote-adapter.pl> <MediaRemoteAdapter.framework> <MediaRemoteAdapterTestClient> stream --no-diff`, actual artifact provenance/hash/license/architecture, required resource paths and Next bundle identity. Debug and packaged resolution must not reach into `_legacy` at runtime or use a shared executable name. No unsupported promise that legacy bundled framework works on macOS 26. Compile/bundle inspection is not live feasibility; default checks spawn no adapter. Live stream/permissions/audio-player interactions need each-run approval.
- Start only accepted enabled/unpaused Bar. One exact owned child, bounded stdout line/total buffered bytes, stderr retention/rate, parse depth/field lengths and queue; UTF-8 fragmentation/multiline chunk handling before JSON framing. Parse wrapper `{ payload: ... }`, validate types and replace complete state, coalesce duplicate snapshots, sanitize diagnostics. Stop cancels reads/backoff, invalidates state first, terminates and reaps exact child/owned descendants if spawned by adapter; prove descendant ownership, never PID-name kill. Late data/exit/restart cannot affect successor. Restart crashes with bounded cancellation-wakeable backoff, not spin; repeated failure reports unavailable and hides Media while Bar stays Running.
- Published DTO is typed, versioned and generation/revision/sequence coherent through 03 bridge; no module locks/native work on read. Malformed/empty payload, child unavailable or no media renders legacy absent state. Preserve title + `-` + artist, `Paused: ` prefix and long-label scrolling exactly; no transport controls or new playback polling loop.
- Source facts: legacy media.rs accepts base64 `artworkData` plus MIME, rejects `<` textual placeholder, decodes header-limited PNG/JPEG/WebP/GIF/BMP, center-crops and Lanczos3 resizes to 128×128 PNG, stores **base64 text in `.txt`**, returns encoded `artwork` string; UI constructs `data:image/png;base64,` and preloads. It does not send a file URL. Preserve displayed image and loading/error behavior; do not promise byte-equal compressed PNG output across encoder upgrades. Current caps: encoded16MiB, decoded12MiB, axis8192, pixels16Mi. Preserve or tighten only with tested compatibility/no visual delta; bound entire stream frame before allocation as well as artwork decode. Cache/encoded read is bounded, atomic/private and Next-identity scoped, no traversal/symlink-following unsafe writes or reuse of legacy cache. Use content identity to prevent title/artist filename collisions; this internal correction must not change valid displayed artwork.
- Asset ownership survives cancellation until decode/native/child work actually cleans up. Ack/reap timeout reports cleanup-pending with retained exact-generation owner and bounded retries; do not equate kill issued with reaped. Host begin-shutdown wakes provider waits before joins; parsing/artwork cannot starve main or host. Empty/error artwork never adds new placeholder styling; test the source's actual preload/retained-art behavior rather than assuming it clears on every missing field.
- Exact fixed source/click/icon map from `Media.constants.ts`: Spotify `com.spotify.client` → SpotifyIcon green22; Microsoft Edge **Dev** `com.microsoft.edgemac.Dev` → YoutubeIcon red22; Tidal `com.tidal.desktop` → SiTidal text18; Feishin `org.jeffvli.feishin` → Vynil02Icon sky22. Unknown player uses PlayCircle02Icon text22, click inert. No stable Edge substitution, generic bundle launcher or icon swap. Host maps validated known identifier to fixed target, never trusts arbitrary player path/name. Extend 05 typed action outcomes and capability additively.

## Acceptance criteria

- [ ] BAR-07-01: Adapter packaging/provenance/macOS 26 feasibility gate is evidenced or explicitly unavailable; Bar never relies on legacy runtime resources/build or name-kill.
- [ ] BAR-07-02: Bounded streaming/parser/artwork/cache and exact-generation child shutdown/reap/restart pass adversarial fake tests without blocking host/main.
- [ ] BAR-07-03: Exact map/icons/labels/preload/hidden/paused states and fixed clicks round-trip typed bridge with independent visual parity and no baseline reset.

## Tests

Fake child/runner streams split UTF-8/JSON, oversized/never-terminated line, invalid wrapper/depth/types, stderr flood, duplicate/empty state, EOF/crash, backoff cancellation, blocked stdout/stderr/reap, stale old exit/completion and successor; assert owned child termination/reap exactly and zero real subprocess. Artwork fixtures: every MIME, malformed base64, encoded/decode/header/pixel caps, `<` placeholder, center crop and deterministic pixels, `.txt` encoded cache read limits, collision/path/symlink cases in temp Next cache. App fake resources missing/wrong architecture/identity, packaged path selection and authorization; no resources taken from legacy. WebKit all map/fallback icons/long strings/artwork success/error/stale-load/paused/absent plus typed launch uncertainty. Actual framework/stream/manual performance results remain gated, never inferred from fakes.

## Notes

Feature/testing/performance docs disclose private-API and hardware limits. No new prompt, player launch, audio mutation or legacy automation in default validation; final merge cannot claim gated media compatibility without evidence.
