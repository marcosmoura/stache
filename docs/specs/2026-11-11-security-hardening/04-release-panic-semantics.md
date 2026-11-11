---
status: draft
---

# 04 — Release panic semantics

## Scope

Make every statement about panic containment true for the shipped `panic = "abort"` release
profile (design point 2, Decision 7). Document what a release panic leaves behind, and pin the
profile with a test so the docs cannot drift. No runtime behavior changes.

## Requirements

- **Inventory.** List every `catch_unwind` site outside tests under `src/crates`. About 40
  files exist, including `sdk/src/ipc/server.rs` (`handle_connection`, "handler panicked"),
  `macos/src/input/system.rs` (`tap_callback`) and `macos/src/ax/observer.rs` (`trampoline`).
  For each, decide whether a nearby comment or doc claims containment without the
  unwind-only qualifier.
- **Comments.** Where a comment promises containment, fail-open behavior or a wire error,
  qualify it once, briefly, that this holds only in unwinding (debug and test) builds and that
  a release build aborts the process. Leave accurate comments and the guards themselves
  alone. The guards stay because they matter in debug and tests and because unwinding across
  `extern "C"` is undefined behavior without them. Do not reformat surrounding code.
- **`docs/architecture/ipc.md`.** Replace "handler panics become wire errors" with the
  truthful version: in unwinding builds a handler panic becomes a `Handler` wire error; in
  release the process aborts, and the client sees the connection close.
- **What a release panic leaves behind.** Add a short "Release panics" subsection to
  `docs/architecture/sdk.md`, next to its existing `catch_unwind` statement, and link it from
  `ipc.md`. Verify each item against the code and the existing docs, and record only verified
  facts, marking anything unverifiable as unverified:
  - the Caps Lock remap, recovered from its journal at the next launch (`keybindings.md`,
    `overview.md`);
  - applications Tiling hid, which stay hidden (check whether any startup path restores them;
    the visibility ledger is runtime-owned);
  - `AXEnhancedUserInterface` leases, which are not restored;
  - the control socket and `server.lock` (stale socket removed at the next start by the probe;
    the kernel releases the lock);
  - the Bar media perl child and keybinding-spawned children, which become orphans (state when
    the adapter exits, for example on its next write to a closed pipe);
  - Keep Awake power assertions, released by the kernel at process exit;
  - border overlays, removed by the window server;
  - anything else found in the inventory.
- **Pin test.** Add a Rust test in an existing test crate that already reads repository
  configuration, beside the `host_policy.rs`-style policy tests. It parses the workspace
  `Cargo.toml` and asserts `[profile.release] panic = "abort"`. Its failure message points at
  the `sdk.md` subsection, so switching to unwind forces the docs to be revisited.
- Under Decision 7's alternative (switch to unwind), stop and re-spec this part with a
  measurement plan.

## Acceptance criteria

- [ ] The inventory is in the commit body: each `catch_unwind` site listed, with whether its
      comment changed.
- [ ] No comment or doc in the active tree (`src/crates`, `docs/` outside archived specs)
      claims panic containment in release builds.
- [ ] `ipc.md` no longer says handler panics become wire errors without the unwind
      qualifier, and it links the new subsection.
- [ ] `sdk.md` "Release panics" lists what an abort leaves behind, each item verified or
      marked unverified.
- [ ] The pin test fails when `panic` is changed to `"unwind"` (checked once locally, then
      reverted) and passes on the real `Cargo.toml`.
- [ ] `bun run lint`, `bun run format:check` and `bun run test` pass. No behavior change: the
      existing panic-containment tests pass unchanged.

## Tests

- The profile pin test above.
- No end-to-end scenario: this part changes no runtime behavior. The existing unwind-build
  tests (`server.rs` "handler panicked", the tap and observer guard tests) remain the
  executable evidence of containment in debug and test builds. Do not add a test that panics
  a release binary.

## Notes

- Archived specs keep their historical wording. Only `docs/architecture/`, `docs/features/`,
  `docs/guides/` and code comments are in scope.
- The audit found no panic reachable from untrusted input. This part is about truthful claims,
  not about new panic paths.
