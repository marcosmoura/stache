---
status: in-progress
---

# 05 — Green Quality run

## Scope

Make the Quality workflow pass on the GitHub `macos-26` runner so Part 01's last criterion can be
judged. Added on 2026-11-13 with the user's approval: Quality is red on `main` for reasons this
spec did not cause, and the user chose to fix them on this branch. Test code and test setup only.
No product behavior changes, and no test is deleted, skipped on CI or weakened to pass.

## Requirements

Run 43358013407 (2026-11-13, this branch after merging `main`) and run 43313313667 (`main`,
2026-11-12) fail the same six `test:ui` tests. Every step before `bun run test` passes.

- **Reduced motion.** The runner reports Reduce Motion enabled. `src/ui/tests/setup.ts` resets
  `emulateMedia({ reducedMotion: null })`, the system default, so tests that expect motion see
  the reduced path: `Media.test.tsx` "scrolls the label only on overflow, at most 560px wide" and
  "narrows the label to 400px in compact mode", `Wifi.test.tsx` "scrolls the network name only
  when it overflows" and "narrows the label in compact mode" (`'truncated'` instead of
  `'scrolling'`). The UI test environment must pin `prefers-reduced-motion: no-preference` as its
  default, from before the first component mounts (Motion reads the query once, see
  `src/ui/tests/reducedMotion.ts`), independent of the machine's setting. Tests that set reduced
  motion themselves keep working. Apply the same default to `test:e2e` if `playwright.config.ts`
  does not already fix it.
- **Keyboard focus.** `BarItem.test.tsx` "shows a visible focus ring for keyboard focus only":
  after `{Tab}`, `document.activeElement` is `body`, not the button. Find the cause (candidates:
  WebKit follows the macOS "Keyboard navigation" setting, which is off on the runner; or the page
  is not focused) and make the test independent of it while it still proves `:focus-visible`
  after keyboard focus and not after pointer focus.
- **Default layout.** `Bar.test.tsx` "renders the registered widgets of the default layout with
  the production registry": the final `vi.waitFor` for one button in the end zone gets zero. Find
  the cause on the runner (candidates: lazy widget load slower than the default `waitFor` timeout;
  a dependence on reduced motion) and fix it without weakening what the test proves.
- **Tiling timing.** `ipc_tests::hops_share_the_request_budget`
  (`src/crates/modules/tiling/src/ipc_tests.rs`) failed in run 43060658983 with `SentUnconfirmed`
  instead of `AdmittedChanged`: a 150 ms actor delay against a 200 ms hop leaves 50 ms for a loaded
  runner. Give it margins that hold on a slow machine while it still proves that two hops share
  one budget.
- **Wall-clock native tests (user decision, 2026-11-13).** Three Quality runs each failed a
  different timing-dependent native test on the loaded runner: `hops_share_the_request_budget`,
  `shutdown::tests::a_blocked_record_sink_cannot_extend_the_terminal_deadline` and
  `module_tests::animation::an_animated_plan_writes_gated_frames_and_acknowledges_the_exact_plan`
  (2 animation frames where at least 4 were expected). The user chose to make the timing tests
  deterministic over CI retries or lower parallelism. Review every native test that asserts on
  real elapsed time or on how much real-time work happened (about 110 `elapsed()` comparisons
  in `src/crates`, plus frame or tick counts driven by real time) and make each hold on a
  machine several times slower than the development machine:
  - where the code under test takes an injected clock, pacer or gate, drive it from the test
    instead of real time;
  - where an upper bound only separates "returned promptly" from "blocked until a much longer
    timeout", raise it to a generous value well under that timeout;
  - where a lower bound or a count depends on real time, lengthen the real duration or loosen the
    count only as far as the property still fails when the behavior is wrong.
    A test whose property cannot be kept without a product change is reported, not weakened.
    The workflow sets `NEXTEST_PROFILE=ci` (the existing no-fail-fast profile) so one run lists
    every failing native test.
- **Later steps.** `test:e2e`, `test:native`, `test:ui:coverage`, the audits, `build`, `tauri:build`
  and the closing lockfile check have not run in CI since the UI rebuild. Failures that appear
  there once the above is fixed belong to this part, under the same rule: fix the cause in tests
  or setup, change no product behavior. A failure that needs a product change or a CI-only skip
  is reported to the orchestrator, not worked around.
- **Docs.** `docs/guides/testing.md` states what the UI test environment pins (reduced motion,
  and whatever the focus fix relies on).

## Acceptance criteria

- [ ] The six named UI tests pass locally and the suite does not depend on the machine's Reduce
      Motion or keyboard-navigation settings (shown by reasoning in the handoff report where a
      system setting cannot be toggled; never change system settings).
- [ ] `hops_share_the_request_budget` has at least 200 ms of slack on each timing edge (was
      50 ms) and a hop at least 4× the elapsed bound, and still fails if hops stop sharing the
      budget.
- [ ] Every native wall-clock assertion was reviewed; the report lists each changed test with the
      reason its property still holds, and the ones left unchanged by group with the reason.
- [ ] No test was deleted, skipped, marked CI-only or had an assertion removed.
- [ ] `bun run lint`, `bun run format:check` and `bun run test` pass locally.
- [ ] A Quality run of the branch passes (the orchestrator pushes and checks). This also closes
      Part 01's last criterion.

## Tests

The changed tests are the tests. End-to-end scenario: the real Quality run on the pushed branch.

## Notes

- The failures cannot be reproduced on the development machine without changing system settings,
  which is not allowed. The CI log is the evidence; each fix states the mechanism it relies on.
- Changed at review (2026-11-13): the first wording asked for 4× between the actor delay and the
  budget. That cannot hold: to prove sharing, the budget must lie between one answer delay and
  two. The criterion now asks for absolute slack instead (400 ms delay, 600 ms budget, 4 s hop).
- Run 43360653833 (2026-11-13): `test:ui` (857), `test:e2e` (62 passed, 26 font skips) and
  `test:scripts` passed on the runner. `test:native` then failed
  `shutdown::tests::a_blocked_record_sink_cannot_extend_the_terminal_deadline` (0.241 s against a
  200 ms wall-clock bound). The sink stays blocked for the whole test, so the bound is now 2 s.
- Wall-clock review outcome (2026-11-13). Changed: 21 sub-second "returned promptly" bounds
  raised to 1–2 s; three tests whose bound sat next to the rig timeout had timeout and bound
  scaled together (`SHORT_LOAD` 1300 ms, a 1120 ms load timeout, a 1300 ms actor delay); one
  positive wait raised to 5 s; two real-time animation counts lowered to the minimum that still
  proves an intermediate frame. Left as they are, because a robust form needs a product change or
  a rig redesign, and still timing-sensitive on a starved machine:
  - `app/src/bar_tests.rs`, two 200 ms bounds against the shared 250 ms rig `BUDGET` (a separate
    budget for those two tests is needed; raising the shared one slows every stalled-hop test);
  - `tiling/src/visibility_tests.rs` `the_settle_never_outlasts_the_restore_hop`, bounded by the
    product constant `RESTORE_SETTLE` (250 ms);
  - `tiling/src/borders/engine_tests.rs` `nothing_to_hide_waits_for_nothing`, bounded by the
    product constant `BORDER_HIDE_WAIT` (32 ms);
  - the real-time animation tests, which have no injectable pacer: a machine starved enough to
    coalesce every intermediate frame would still fail them;
  - `app/src/popover_actions_tests.rs` `drain()`, whose 200 ms idle receive is its idle detector.
