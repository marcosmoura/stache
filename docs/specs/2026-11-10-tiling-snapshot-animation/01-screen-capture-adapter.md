---
status: draft
---

# 01 — Screen Recording capability and window capture adapter

Read when implementing the capture adapter. **DRAFT — NOT SHIPPED.** This part needs separate approval. Native checks belong to part 05 (N2, N3) and each needs [per-run approval](../2026-11-03-tiling/validation.md#per-run-native-approval). Paths are under `src/crates/` unless noted.

## Scope

- New `macos/src/screen_capture.rs` or `macos/src/screen_capture/` (the implementer chooses), exported from `macos/src/lib.rs`.
- A new dependency, `objc2-screen-capture-kit`, in `macos/Cargo.toml`, plus any `objc2-core-graphics` features it needs (`macos/Cargo.toml:26`). Workspace and lock files change to match.
- `NSScreenCaptureUsageDescription` in the app bundle: an `Info.plist` next to `app/tauri.conf.json` that Tauri merges, or the equivalent Tauri config key.
- A fake for tests.
- Nothing in Tiling uses this part yet.

## Requirements

- **Dependency.** Before adding `objc2-screen-capture-kit`, verify with Context7 or crates.io:
  - the version in the 0.3.x family that matches the existing `objc2-*` 0.3.2 crates (`macos/Cargo.toml:21-29`);
  - the exact features needed for `SCShareableContent`, `SCWindow`, `SCContentFilter`, `SCStreamConfiguration` and `SCScreenshotManager`.

  Use `default-features = false` and only the needed features, like the other `objc2-*` entries. Record the chosen version and features in Notes.

- **Capability.** Add a `Send + Sync + Copy` capability enum for status, for example `ScreenCaptureCapability { Granted, NotGranted, Unavailable }`. `Unavailable` covers a missing framework or symbol and an OS without `SCScreenshotManager`.
  - `preflight()` calls `CGPreflightScreenCaptureAccess`. It never prompts and never blocks main.
  - The result is cached for at most 1 s, so a grant made during the session is picked up if macOS allows it without a relaunch.
- **Request once.** `request_once()` calls `CGRequestScreenCaptureAccess` at most **once per app session**. A process-lifetime flag owned by the adapter instance guarantees this; it is not a global static. Later calls return the current preflight result without prompting.
  - The thread the request runs on is decided by native gate N3. Until then it never runs on an AX owner thread and never holds a lock.
  - The adapter reports whether a request was already made this session.
- **Usage string (OQ-4, resolved: add the key anyway; N3 verifies).** The bundle carries `NSScreenCaptureUsageDescription` with a one-sentence reason: Stache captures the images of windows it is about to move so it can animate them smoothly.
  - Verify with Context7 how Tauri 2 merges `Info.plist` keys.
  - Record what Apple documents for the key on macOS 26 (OQ-4).
  - A test checks that the bundled plist source contains the key.
- **Capture.** `capture(windows: &[u32], deadline: Instant, reply)` is an asynchronous single-window screenshot per `CGWindowID`.
  - Find each `SCWindow` by `windowID` in **at most one** `SCShareableContent` query per call, not one per window.
  - Build a desktop-independent single-window `SCContentFilter` for each window.
  - Size the `SCStreamConfiguration` to the window's frame at its display's backing scale, with no cursor and no shadow.
  - Call `SCScreenshotManager` to take the image.
  - All captures of one call run concurrently. Concurrency is bounded at 16 windows per call (matching `MAX_ANIMATED`) and 32 in flight per adapter. A call over either bound answers the extra windows `Busy` without capturing them.
  - The reply runs **off main** and exactly once per window, with one of:
    - `Image(WindowImage)`;
    - `Missed` (deadline passed; a late image is dropped, never delivered);
    - `NotFound` (no `SCWindow` with that id);
    - `NotGranted`;
    - `Failed(code)`;
    - `Busy`.
  - The reply never blocks, and no lock is held across it.
  - A window that is mostly off-screen (parked by part 04) is captured like any other window. If ScreenCaptureKit returns an error or an empty image for it, the reply is `Failed(code)`; part 04 then keeps the start image. Native gate N8 records the real behavior.
- **Image ownership.** `WindowImage` owns one retained `CGImage` plus its pixel size and scale.
  - It is `Send`, so it may move to main for a layer's contents, and it is released exactly once when dropped.
  - It has no `Debug` output of pixels. It is never written to disk, logged, serialized or sent over IPC.
  - Only the window ids passed by the caller are captured.
- **Fake.** `testing::FakeScreenCapture` (gated `#[cfg(any(test, feature = "testing"))]`, like `macos/src/hold_hud.rs:180-181`) has:
  - a settable capability and request outcome, counting requests;
  - per-window scripted latency, result or miss;
  - a call log;
  - a held-reply mode for deadline tests;
  - a fake image type with no real `CGImage`.

  Expose the adapter to callers through a trait so Tiling can inject the fake.

- Default tests never call ScreenCaptureKit, `CGPreflightScreenCaptureAccess` or `CGRequestScreenCaptureAccess`.

## Acceptance criteria

- [ ] SA-01-01: `objc2-screen-capture-kit` is added with a verified 0.3.x version and minimal features recorded in Notes. `Cargo.lock` builds with `bun run lint` passing.
- [ ] SA-01-02: The capability enum, the cached non-prompting preflight and `request_once` exist. A request is issued at most once per adapter lifetime (tested on the fake and on the real adapter's guard logic with an injected native function).
- [ ] SA-01-03: The bundle carries `NSScreenCaptureUsageDescription`, a test asserts it, and Notes record how Tauri merges it.
- [ ] SA-01-04: Capture by `CGWindowID` is asynchronous with a deadline, one shareable-content query per call and bounded concurrency. Each window gets exactly one typed reply off main, and a late image is dropped.
- [ ] SA-01-05: `WindowImage` ownership is release-once and `Send`, it never logs pixels, and the fake and trait are available to Tiling tests.

## Tests

- Fake: scripted results per window (including a scripted failure for a parked window), a miss at the deadline with the late image dropped, a single reply per window, `Busy` past the bounds, `NotGranted`, and the request count staying at one after repeated `request_once` calls.
- Real adapter guard logic with injected native function pointers: request-once, the preflight cache expiring after 1 s, and the bounds counter released on every reply path, including the miss path.
- A drop-count test for `WindowImage` through the fake image type.
- The plist key test.

## Notes

- Ad-hoc signed dev builds (`app/tauri.conf.json:25`, `signingIdentity: "-"`) may lose the grant after each rebuild because the code signature changes. Under `bun run tauri:dev` the process may not be the bundle, so TCC may attribute the request to the terminal. macOS may need a relaunch after a grant. N3 records all three.
- Rollback: remove the module and the dependency. Nothing else uses them.
