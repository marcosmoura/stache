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
