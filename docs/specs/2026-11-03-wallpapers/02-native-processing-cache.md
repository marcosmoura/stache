---
status: done
---

# 02 — Native processing and cache

Read when implementing the planned adapter and image pipeline. Depends on Part 01.

## Scope

Add the safe injected Wallpapers adapter under `src/crates/macos/src/`, pure
processing/cache services under `src/crates/modules/wallpapers/src/`, and small
generic SDK/AppPaths cache coordination integrated into
`src/crates/cli/src/cache.rs`. No production rotation or CLI wallpaper command
is enabled yet. Tests never call the real native setter.

## Requirements

### Native compile-proof checkpoint

- Before pipeline integration, compile-prove installed objc2 bindings/features,
  AppKit initialization, stable native display identity, geometry and setter
  ownership for desktop and standalone CLI. Record evidence and chosen safe
  signatures in adapter Rustdoc; do not assume a preexisting wallpaper API.
- `objc2-app-kit 0.3.2` generated `NSScreen` is `MainThreadOnly` and
  `screens` requires `MainThreadMarker`. Keep AppKit/screen objects on main;
  pass owned plain identity/geometry/path/result data to workers. No blanket
  raw-pointer `Send`/`Sync`. The generated `NSWorkspace` setter is unsafe
  because its options dictionary must contain appropriate types: isolate and
  explain that proof in macOS, exposing a safe injected interface to the module.
- Use public native per-display desktop-image operations for all targets, not
  the legacy all-screen `wallpaper` crate/osascript/System Events behavior.
  Declare no required Automation/Accessibility permission or private fallback.
- Snapshot ordered screens, stable identity, frame and scale once per operation;
  compute checked positive finite target pixel dimensions for each display.
  `main` means `NSScreen.screens[0]`, not focus-dependent `mainScreen`.
  Numeric selection is 1-based; retain legacy `0` alias for primary. Reject
  out-of-range, absent/invalid geometry before decoding or cache writes; no
  invented fallback display/resolution. Check again by identity/geometry before
  native side effects; hotplug must not retarget an index to another screen.
- Dispatcher acknowledgment/cancellation waits are bounded and late invalidated
  jobs inert. Never wait on a worker from main when it can wait on main.
  Standalone may initialize only narrowly required AppKit on its process main
  thread and pump/dispatch as proved necessary, never Tauri, windows, tray,
  host listener or modules. If safe ownership cannot be proved, stop and report
  the blocker instead of replacing it with guessed signatures.

### Processing contract

- Real JPEG/PNG/WebP decoding must match the four advertised extensions. Cover
  resize with Lanczos3, center crop to exact per-display pixels, then blur,
  then black rounded corners. Retain Gaussian blur for radii at most 5 and
  native-resolution fast-blur approximation above 5; corners cap radius to half
  the smaller dimension and use 8×8 supersampling. Encode RGB8 lossless PNG with
  Best compression and adaptive filtering (or verified equivalent if the
  selected encoder API differs). Source bytes are never rewritten/deleted.
- Bound decode bytes, dimensions/pixels, intermediate allocations and total
  concurrent work with checked arithmetic and decoder limits. During the
  checkpoint choose/document concrete internal limits and test boundary values;
  no public knobs or arbitrary rejection of ordinary existing inputs. Enormous
  sources/effects fail with resource errors before allocation/work explosion.
- Use bounded workers/backpressure, not a materialized unbounded
  screens×images job queue. Observe cancellation at queue/decode/resize/effects/
  encode/publication/native boundaries; acknowledge noninterruptible codec work
  honestly. All requested outputs must be successfully prepared and validated
  before any native set call. Processing failure changes no desktop.
- Native application can partially succeed. Return a typed outcome carrying
  known applied, failed, not-attempted and unknown target identities; no all-success response
  for partial work or impossible rollback guarantee. Stop further calls after
  failure/cancellation. Already-issued requests can finish, but cannot update
  successor config/state or initiate another change.

### Cache and clear

- Store immutable generated outputs beneath `AppPaths`' Next cache
  `wallpapers` subdirectory, never legacy/home fallbacks. Cache identity includes
  canonical source identity, a digest of the bounded source bytes actually
  processed, target pixel dimensions, effects and processing version. Hashing
  occurs for processing/cache lookup, not simple `list`. Same-length/same-mtime
  changed bytes must invalidate because bytes are hashed; metadata alone is not
  a content guarantee. Detect source replacement during processing so a key
  never describes different bytes than its output.
- Validate existing cache outputs (owned regular file, valid PNG, expected
  dimensions/output contract) and rebuild corrupt/truncated entries. Publish a
  completed, closed/validated same-directory temporary atomically; no reader
  sees a partial final. Concurrent publishers may reuse an equivalent winner.
  Failures/cancellation remove only owned temporary artifacts, not sources.
- Add a small generic cache lease primitive in SDK/AppPaths: shared leases for
  processing/publication/read/apply and an exclusive lease for whole-root
  `cache clear`. The same-user owned, no-follow lock lives outside the cache
  root, so ordinary cache deletion/recreation retains lock identity. Participants
  never unlink the lock file, including clear or normal shutdown. Validate private
  ownership/no-follow metadata and record directory/lock identity; recheck path
  versus held inode before critical work. Replacement fails closed or safely
  reacquires only before critical work, never continues under a split identity.
  OS/temp-cleaner removal can break advisory coordination; detected removal or
  replacement refuses work until safely reacquired. This is not universal
  protection against uncooperating deletion or malicious same-UID mutation.
  Use safe standard
  locking/rustix facilities, consistent ordering and bounded cancellable waits;
  do not upgrade a held shared lock or hold native/registry metadata locks while
  waiting. This is coordination among cooperating processes, not a distributed
  scheduler or protection against arbitrary hostile same-UID mutation.
- Exclusive clear waits at most five seconds, cancellably, using a local lease
  budget (aligned with the existing client I/O window for a familiar finite wait,
  not a global processing deadline). On busy expiry, mutate nothing, exit 1 and
  print exactly `stache: Failed to clear cache: cache is busy; retry after current operations finish.`
  plus newline through the existing fail prefix. Cancellation also mutates
  nothing and exits 1 with exactly `stache: Failed to clear cache: operation cancelled.`
  plus newline; the cancellable lease/error mapping is planned, not a shipped
  cache command capability. Successful legacy clear text remains unchanged;
  no infinite wait or public knob.
- Validate ownership/no-follow directory and entry metadata before use; reject
  symlinked/foreign cache destinations. Clearing preserves existing cache
  path/clear output and exit behavior, removes the whole root only under its
  exclusive lease, and leaves runtime socket/config/legacy identity untouched.
  A later operation safely recreates the cache. No stale process-global cache
  state. Hold apply leases through native acknowledgment; state what an
  already-issued operation means when cancellation races its completion.
- Do not promise that assigning an OS wallpaper URL makes its cache file
  permanent. After clear, OS ownership/persistence is system-dependent; document
  observed behavior honestly. No automatic restore, recache-on-clear, new
  ApplicationSupport retention scheme or eviction feature.

## Acceptance criteria

- [ ] WP02-01: macOS native wrapper and standalone thread/dispatch proof compile; module
      remains unsafe-free; fake adapter is the default in automated tests.
- [ ] WP02-02: Per-display dimensions, selectors and hotplug validation fail before work
      when invalid; every output is prepared before the first setter call.
- [ ] WP02-03: Pixel/effect/format tests and bounded resource/cancellation tests pass.
- [ ] WP02-04: Immutable cache keys/publication/corruption recovery and cross-process
      clear leases pass failpoint tests without source modification.
- [ ] WP02-05: Partial native errors report truth; cancellation cannot enqueue follow-on
      work or publish obsolete state. Docs and overview gates pass.

## Tests

Fixtures for real JPEG/JPEG-extension/PNG/WebP, corrupt/oversized input, wide/
tall/odd cover crops, 1×/2× mixed displays, zero effects, blur 5/6, radius cap,
black/antialiased corners and RGB8 PNG. Fake geometry covers primary versus
focused screen, 0/1/N selectors, no displays, overflow, hotplug/reorder/scale
change and invalid selector before any processing/cache write.

Cache tests: same basename different directories; changed bytes with unchanged
mtime/length; geometry/effects/version changes; corrupt cache; failed write,
flush and publish; concurrent equivalent producers; bounded work cancellation.
Separate disposable child processes test lease contention, clear during
write/publish/apply/read, cancelled waits, process exit, missing root/recreation,
symlink/foreign-path denial, held-inode/path replacement, no unlink at shutdown/
clear, fake-time five-second busy expiry and cancellation with zero mutation,
and retained cache CLI goldens. Fake native tests
prove processing failure causes zero calls, all-prepared ordering, partial
failure identity reports and late completion suppression.

## Notes

Stache does not support macOS Spaces. Native Space coverage is not a test,
verification gate or future functionality; Tiling's app-level virtual workspaces
are unrelated. No automated test, including a “smoke test”, may call a real
setter or restore an image. Each real desktop run requires explicit approval.

### Verification evidence — November 5, 2026

- Automated checks exercise real `NSScreen` resolution/recheck with simulated
  expected geometry/scale mismatches, measure a cap of four workers, and prove
  cross-process busy results at write/sync/publish and batch-processing pauses.
  No automated test calls the real setter; simulated mismatches are not physical
  hotplug evidence.
- An approved standalone process ran on its main thread with one attached native
  display: id `3`, 2560×1440 points and pixels, scale `1`.
  `NativeDesktop::set_image` returned `Ok` in about 1.75 ms for a generated RGB8
  magenta PNG in a disposable temporary directory. The user visually confirmed
  solid magenta. Immediate `NSWorkspace.desktopImageURLForScreen` readback still
  showed the old URL: immediate URL readback is not reliable evidence of visual
  completion.
- Only that probe image was deleted. The user confirmed magenta remained
  visible. This simulated cache-file loss, not actual `cache clear` integration;
  no real cache was touched and no long-term persistence was established.
- Restoration via the recorded original URL and exact AppKit options returned
  `Ok`. Immediate readback still showed the probe URL, but the user visually
  confirmed the original restored. The probe exited `0`. This does not establish
  perfect restoration of dynamic wallpaper or any stronger native guarantee.
- Logout/restart, long-term persistence, physical hotplug, multi-display behavior
  and actual cache-clear desktop behavior were not checked. Those observations
  remain unverified; OS ownership/persistence after removal is system-dependent.
  This evidence changes no part status and does not authorize Part 03.
