---
status: draft
---

# 03 — Decision record

Read when implementing after parts 01 and 02 are done, after separate approval, only when the owner explicitly selects this part and every other roadmap workstream is finished or has no actionable part ([priority](00-overview.md#priority)).

## Scope

A decision document at `docs/specs/2026-11-10-gpui-evaluation/decision.md`, linked from the [roadmap](../../roadmap.md) row, that compares the evidence and gives a go/no-go recommendation (overview Decision 7). This part also decides whether `spikes/gpui/` is kept or removed. No code outside `spikes/` and the root ignore entries changes.

## Requirements

- **Comparison:** per metric (RSS, idle CPU at the shared cadence and wakeups as app totals), part 01 versus part 02 with medians, min/max and each side's per-metric noise envelope. App totals include attributed WebKit helpers (part 01 [helper attribution](01-baseline-measurements.md#webkit-helper-attribution)); host-only values appear only in the per-process breakdown. Only valid trials are compared; excluded trials and their reasons are shown ([exclude and replace](01-baseline-measurements.md#method)), and a `FAILED` run is not used.
- **Startup, surface detection and first frame** are reported per side with each side's definition (part 01 [surface detection](01-baseline-measurements.md#surface-and-visibility): attributed WebContent helper for Stache Next, `spike: ready` for the spike), labelled "not directly comparable". Only RSS, CPU and wakeups surface cost enter the within-noise comparison.
- **Primary result: surface cost** = `surface-visible` − `no-window` per metric on each side (helpers included). Absolute app totals are shown separately and labelled as different process models (full Stache Next host versus minimal spike), never as the headline.
- A difference smaller than the larger of the two envelopes for that metric is reported as "no measurable difference"; for surface cost, a side's envelope is the sum of its `no-window` and `surface-visible` envelopes. Unverified metrics stay unverified (open-to-content and first frame for Stache Next unless OQ-2 was approved). A spike scenario at a cadence part 01 did not record (labelled extra scenario) is shown separately and never compared against the baseline.
- **Window behavior:** all fifteen part 02 checklist outcomes, including hover/tracking while not key, accept-first-mouse, `fullScreenAuxiliary`, the legacy visual-parity probe and VoiceOver support. Each **works with patch**, **blocked** or **not observable** item is listed with its effect on Bar; a missing accessibility tree counts as a migration risk.
- **Migration effort**, per subsystem, with files and an **estimate** in parts (not days), labelled as an estimate: host and tray (`Builder`, tray/menu), windows (`NativeWindows`, the test and access surfaces, Bar if shipped), components and theme (tokens, Linaria styles, icons, visual parity), accessibility, tests (Vitest + Playwright WebKit replacement), bindings removal (`tauri-specta`, generated files, drift checks), build and release (bundle, signing, `tauri:build` replacement), and docs/`AGENTS.md` updates. Include the process model from the part 02 coexistence result.
- **Maintenance risk:** crates.io staleness, the chosen pin's update path, API churn between pinned revs, license, upstream issue activity, patches part 02 needed, and the spike's unaudited dependencies.
- **Recommendation:** go or no-go with the reasons tied to evidence. "Go" only recommends writing a separate migration spec for approval; it authorizes nothing. "No-go" records the conditions that would justify re-evaluating.
- **Spike disposition:** keep (with a reason and an owner) or remove `spikes/gpui/` and its root ignore entries; root checks stay green either way.
- **Roadmap:** the GPUI evaluation row links `decision.md` and states the outcome.

## Acceptance criteria

- [ ] GE-03-01: `decision.md` exists in this spec directory and follows the [doc style](../../index.md#doc-style). Every performance metric is traceable to a linked `samples.json`; every build fact is traceable to a command and its output recorded in `spikes/gpui/README.md`; every effort figure is marked as an estimate.
- [ ] GE-03-02: The document states go or no-go, the comparison within per-metric noise, all fifteen checklist outcomes, the effort table, the risk assessment and the spike disposition.
- [ ] GE-03-03: [docs/roadmap.md](../../roadmap.md) links `decision.md` from the GPUI evaluation row and shows the outcome.
- [ ] GE-03-04: `bun run lint`, `bun run format:check` and `bun run test` pass after the spike disposition.

## Tests

Root checks after the disposition. A reviewer checks each performance number against the samples, each build fact against the README, and each checklist claim against part 02's recorded outcomes.

## Notes

- If Bar shipped in React between part 01 and this part, part 01 is re-run against Bar before this part is accepted.
