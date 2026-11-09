---
branch: feat/gpui-evaluation
---

# GPUI evaluation

Read when planning or running the evaluation of GPUI as a replacement for the Tauri 2 + WKWebView/React UI layer. **DRAFT — NOT SHIPPED.** Every part needs separate approval, and every native run (launching Stache Next or the spike for a measurement or a window check) needs its own explicit owner approval. This document authorizes no native run and no migration.

## Goal

Produce evidence and a go/no-go decision on whether replacing the Tauri 2 + WKWebView/React UI layer with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) (Zed's Rust GPU UI framework) is worth it. Trigger: on 2026-11-04 Discord announced it is rebuilding its desktop client on a patched GPUI. The output is measured baseline numbers, a prototype checklist and a decision record. It is **not** a migration.

## Priority

**Always bottom priority** (owner decision, 2026-11-10). A part of this spec starts only when **both** hold:

1. The owner explicitly selects it.
2. Every other [roadmap](../../roadmap.md) workstream, including Release and Tray Reload, is finished or has no actionable part: each remaining part is blocked on an external gate (for example signing credentials or a recorded approval that has not been given).

Selecting a part of another workstream pauses this spec at the next part boundary; a part in progress here is finished or left `in-progress` on its own branch, never merged unfinished. This spec never blocks, reorders or gates other work, and no spec, part, branch or decision may depend on it or wait for its outcome.

## Context (established facts)

- **Thin current UI.** The tray, Hold-to-Quit HUD, MenuAnywhere menu and Tiling borders are already native AppKit/Core Animation in `stache-macos`. React has three windows in `src/ui/src/shell/windows/` (`AccessibilityGuide`, `Placeholder`, `TestWindow`). Tauri is referenced in about nine Rust files: the `Builder`, tray/menu, about five commands, one `WebviewWindow` path and the `tauri-specta` bindings. See [architecture/ui.md](../../architecture/ui.md).
- **Bar is the large planned webview surface.** [Bar](../2026-11-03-bar/00-overview.md) requires legacy visual parity and runs on the primary display only (its Decision 3). If Bar has shipped in React when this spec runs, the evaluation measures the Bar window and includes the cost of porting Bar.
- **No Chromium to remove.** On macOS Tauri uses system WebKit, not a bundled Chromium, so Discord's Electron→GPUI memory gain does not transfer directly. Expected gains, all to be measured: per-webview WebContent/GPU/Networking helper memory, CPU and wakeups (attributed to the app by part 01's quiet-system snapshot diff, because these XPC helpers are `launchd`-parented and never in the host's process tree), host idle CPU and wakeups, IPC serialization, first-frame latency (unverified for Stache Next unless OQ-2 is approved; never compared within noise), and a single-language toolchain.
- **No page-loaded signal in release builds.** The only `on_page_load` hook is behind `#[cfg(feature = "smoke")]` in `src/crates/app/src/windows.rs` (`TauriWindowHost::open`); `tauri:build` does not enable `smoke`. Window-open-to-content for Stache Next release builds is therefore **unverified** unless the owner approves instrumentation (OQ-2).
- **Accessibility gate.** A launch without Accessibility trust opens the guidance webview while the startup line still reads `webviews=0`, then logs `access: guidance window requested` ([guides/performance.md](../../guides/performance.md), [Accessibility launch gate](../2026-11-07-accessibility-gate/00-overview.md)). `core.status` carries `access.state` (`checking`, `blocked`, `open`). Measurements require a trusted bundle.
- **Known risks** (each to be verified, not assumed):
  - The official `gpui` crate on crates.io is stale (0.2.2, 2025-10-22; zed-industries/zed#66275). Alternatives are a git pin of `zed-industries/zed` or a community republish (`gpui-unofficial`, `gpui-ce`, `xgpui`).
  - GPUI and Tauri/tao both want to own `NSApplication` and the main run loop, so running both in one process is likely impossible. Part 02 tests this explicitly.
  - Bar needs a non-activating panel, all Spaces + stationary, a custom window level (below normal), click-through, accept-first-mouse, hover tracking while not key, blur/vibrancy, correct display placement and a screen reservation. GPUI may need raw `NSWindow` access or patches for some of these.
  - Accessibility (VoiceOver) support in GPUI is unproven; WKWebView exposes the DOM accessibility tree today.
  - The Vitest + Playwright WebKit suites and the `tauri-specta` binding drift checks do not carry over; a migration would replace them.

## Non-goals

- Migrating, changing or removing any shipped surface (tray, windows, HUD, menus, borders, React UI, bindings).
- Adding GPUI to, or changing any Tauri pin in, the shipping `stache-app` binary, the root Cargo workspace members or its default members.
- Changing the signature or behavior of any existing export in `scripts/measure/*`, or the foundation `bun run measure` tool.
- Writing a migration spec. A "go" outcome only recommends that a separate migration spec be written and approved.
- Launching legacy automation, or measuring legacy Stache. Legacy is not a baseline here.

## Decisions

1. **Standalone spike.** The prototype lives in a standalone Cargo project at `spikes/gpui/`, outside the root workspace, with GPUI pinned to an exact git rev or an exact version of one republish. Part 02 records the choice and why.
2. **Root checks ignore `spikes/`.** Only the root inputs that would otherwise see the spike change (part 02 lists them and the ones verified unneeded). Every root check stays green. The spike is otherwise ungated: its README carries manual `cargo fmt --check` and `cargo clippy` commands, which is acceptable for a throwaway spike.
3. **Measurement method** follows Bar's performance rule ([Bar overview](../2026-11-03-bar/00-overview.md), "Performance"): identical hardware, window size and placement, update cadence, warmup, duration and trial count on both sides; at least 5 idle trials per scenario, each a fresh launch; publish raw samples and the method; report a per-metric noise envelope. Never fabricate a number; anything not measured stays **unverified**. Each native run needs explicit owner approval.
4. **Comparable surfaces and cadence.** Part 01 measures the Bar window when it exists, otherwise the test window (OQ-1), and records its update cadence (`none` for a static window). Part 02's spike runs with exactly that cadence (`--cadence none|1s`) and supports the same two scenarios (`--window none|surface`), showing its tray icon in both, so the surface cost is a no-window delta on both sides. When part 01 is static, the spike is measured static; a 1 s spike run may be added only as a separately labelled extra scenario and is never compared against a static baseline. A difference in shape between the two surfaces is recorded as a limitation, never normalized away.
5. **Measurement tooling reuses the foundation tool's safety rules and target-agnostic modules** ([guides/performance.md](../../guides/performance.md)) without changing them: release builds only, isolated temporary roots, refusal while another Stache Next, legacy Stache or spike instance runs, signals only to owned children, no name-based termination, `report.md` + `samples.json` kept even on failure. Target launch, readiness, WebKit helper attribution and sampling, output directory and report rendering are new code (part 01).
6. **Distinct identities.** The spike uses bundle identifier `com.marcosmoura.stache.spike.gpui` and the coexistence binary `com.marcosmoura.stache.spike.coexist`; neither product name contains "Stache". A bare Cargo binary has no bundle identifier, so each is built as a `.app` bundle or embeds an `Info.plist` (`-sectcreate __TEXT __info_plist`); the measured spike is a `.app` (part 02). Neither reads Stache configuration, socket or cache paths.
7. **Decision record location.** Part 03 writes `decision.md` in this spec directory and the roadmap row links it. No lasting doc under `docs/architecture/` is added for a non-shipped evaluation.

## Parts

| Part                              | Summary                                                                                                                              | Status |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ------ |
| [01](01-baseline-measurements.md) | New `measure:ui` tool and baseline of the current Tauri surface: app totals (host + attributed WebKit helpers), startup, gate, noise | draft  |
| [02](02-gpui-prototype.md)        | Standalone `spikes/gpui/` Bar-like window, window-behavior checklist, tray, Tauri coexistence, same measurements                     | draft  |
| [03](03-decision-record.md)       | `decision.md`: compare within noise, migration effort per subsystem, GPUI maintenance risk, go/no-go, spike kept or removed          | draft  |

## Open questions

- **OQ-1 (part 01, Bar not shipped).** The only webview reachable without a host change is the tray's **Open Test Window**: one decorated `Standard` window per owner/key, not bar-sized and not one per display. Proposed default: measure it as-is on the primary display and record the shape as a limitation. Alternative needing approval: a measurement-only `stache-app` cargo feature (off by default, never in `tauri:build`) that opens a bar-sized test surface.
- **OQ-2 (part 01, open-to-content and first frame).** Release builds have no page-loaded or first-frame signal (the `on_page_load` hook is `smoke`-only). Proposed default: report only signals that already exist (`startup: tray ready`, `core.status` readiness, the appearance of the attributed WebContent helper used to detect the surface) and mark window-open-to-content and first frame **unverified** for Stache Next. Adding an instrumentation log line to `stache-app` needs approval.
- **OQ-3 (part 02, `AGENTS.md`).** `AGENTS.md` says new sources live in `src/crates/` and `src/ui/`. Proposed: part 02 adds one line naming `spikes/` as an excluded, non-shipping evaluation area.

## Affected docs

- Now: [docs/roadmap.md](../../roadmap.md) (snapshot date, bottom status row and priority sentence).
- Part 01: [guides/performance.md](../../guides/performance.md) (UI measurement section) and `measurements/` under this spec.
- Part 02: [guides/testing.md](../../guides/testing.md) (root-check exclusions and the spike dev-watch check), `AGENTS.md` (OQ-3), `spikes/gpui/README.md`.
- Part 03: `decision.md` in this spec directory; roadmap row links it and shows the outcome.
