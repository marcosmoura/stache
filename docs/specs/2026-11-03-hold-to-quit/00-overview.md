---
branch: feat/hold-to-quit
---

# Hold-to-Quit rewrite

Read when planning the independent Hold-to-Quit (`commandQuit`) module. This is a draft, not shipped behavior or permission to implement.

## Goal

Rebuild legacy “hold ⌘Q to quit” as `stache-hold-to-quit`: a default-disabled module that suppresses a Command-only ⌘Q, shows a native progress HUD and asks the exact application instance that was frontmost at key-down to quit gracefully once the hold reaches `holdDuration`. Fix the legacy tap, pause and timer bugs. Ask for approval before implementing each part.

## Non-goals

- No private event tap. Input comes only from the shared SDK input-tap service (see Decisions).
- No exclusion list or any new config key. No `forceTerminate`, no webview/React HUD, no Settings UI, no CLI command and no module-specific mutating endpoint.
- No fix for Stache's own Cmd-Q/logout coordinated-teardown gap, which is separate.
- No changes to proxyAudio, noTunes policy or Keybindings/MenuAnywhere behavior beyond additive adapter extensions.

## Decisions

These were approved by the user and are fixed.

1. Crate `src/crates/modules/hold-to-quit`, package `stache-hold-to-quit`, module/catalog ID `commandQuit`, file `commandQuit.jsonc`. Only keys: `enabled` (default `false`) and `holdDuration` (milliseconds, default `1500`). Preserve every `u64`, including zero and `u64::MAX`; no arbitrary bounds or clamping. Checked deadline arithmetic must not overflow; an unrepresentable deadline is a cancellable indefinite wait. Zero is eligible immediately, but only after matching HUD-show acknowledgement and current-state checks. Migration reuses the shipped catalog path.
2. Input uses the upcoming shared host SDK input-tap service from the [Keybindings spec](../2026-11-03-keybindings/00-overview.md) (draft on `main`, implementation branch `feat/keybindings`). The host provides it through `ModuleContext`; it takes a declarative filter and a pure-decision callback (immutable event snapshot → pass/suppress from precomputed state, all work queued to worker/main). It has one `stache-macos` owner tap, fixed priority Hold-to-Quit > MenuAnywhere > Keybindings, overlap diagnostics and automatic re-enable. Part 04 is **blocked** until that service and the additive pairing/conflict gate are on `main`.
3. Quit means graceful `terminate` only, never `forceTerminate`. `Refused`, `NotRunning` and `Stale` are reported as such. The instance captured at ⌘Q-down is the only one that can be terminated. If the frontmost application changes during the hold, the press is cancelled: the HUD hides, nothing is terminated, and the rest of that physical press (repeats and key-up) is swallowed.
4. When Finder (`com.apple.finder`) or Stache's own process is frontmost, ⌘Q passes through unswallowed and gets no hold behavior. Stache is never terminated through this path.
5. A native HUD is required before interception: hidden installation/readiness is acknowledged before Running; failure means `Unavailable` and pass-through, never intentional invisible interception. The `stache-macos` HUD is a non-activating, click-through `NSPanel`, shown only from queued down work, with progress and “Hold ⌘Q to quit <App>”. Only a matching successful show acknowledgement arms termination. It hides on release, quit request, cancellation and lifecycle changes. Main owns all Cocoa objects; opaque `Send` handles carry no native object. Placement targets the frontmost app's display when public APIs provide geometry, otherwise reports its fallback. Part 02's minimal native design is an engineering proposal, not user-approved pixel values.
6. The legacy bugs are fixed. The shared service re-enables the tap. Pause, disable and stop invalidate any in-flight press, deadline and HUD immediately, so resume never quits from a stale press. The deadline is an owned, cancellation-wakeable wait, never idle polling.
7. Tests never install taps, terminate real applications, show real panels or prompt for permissions. Manual native gates need explicit approval for each run.
8. The Cmd-Q/logout teardown gap stays out of scope.
9. Match physical keycode 12 and Command-only (no Shift/Control/Option), not layout text. No new socket endpoint, including read-only status, and no new CLI. Callback decisions use only a prepublished generation-tagged nonblocking snapshot; no native work or application query there. Missing, invalidated or unconfirmed frontmost/exemption/HUD readiness fails open. Main revalidates exact frontmost identity before show and terminate; notification lag is an explicit limitation, not perfect OS/input atomicity.
10. The shared host input service must retain physical-press pairing through cancellation and register conflicts with Keybindings' ordinary Carbon hotkeys as well as HID filters. HID priority alone cannot prevent an ordinary Cmd-Q binding bypass. Part 04 owns the additive integration gate, not a competing framework.

## Parts

| Part                               | Summary                                                                                                 | Status |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------- | ------ |
| [01](01-config.md)                 | Typed config, catalog, schema, bindings and typed migration                                             | draft  |
| [02](02-native-hud.md)             | Native HUD adapter, fakes and ownership feasibility gate                                                | draft  |
| [03](03-applications-frontmost.md) | Additive `applications` extension: frontmost observation/query, graceful terminate, self/display lookup | draft  |
| [04](04-runtime.md)                | Runtime/factory over the shared input service: hold state machine, status, tray, teardown, race tests   | draft  |

Order and blocking: inspected `main` is `ad34b46`, including shipped noTunes (`ccae030`), the applications adapter and catalog migration hook. This worktree's implementation base is older (`e0b4648`); rebase before implementing, not during this docs-only task. 01 reuses that migration; 02 is independent; 03 extends the shipped adapter; 04 needs 01–03 plus the upcoming Keybindings host input service and its conflict/press-pairing integration. Each part builds, tests and commits independently. Only the orchestrator changes status; all four remain draft and block merge.

## Integration order

The roadmap order is noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → Bar → Tiling. noTunes ships; [Keep Awake](../2026-11-03-keep-awake/00-overview.md) and [Wallpapers](../2026-11-03-wallpapers/00-overview.md) are drafts on `main`, with implementation on `feat/keep-awake` and `feat/wallpapers`. Rebase each part onto current `main`, reuse landed helpers/context/input services and preserve additive catalog/bindings/index/roadmap routes. No dependencies on unmerged module crates. Adapter extensions leave noTunes' force API and tests unchanged.

The [consolidated Wallpapers audit](../2026-11-03-wallpapers/review-findings.md) (`git show f13cd3a:docs/specs/2026-11-03-wallpapers/review-findings.md`) informs these contracts: C02 immediate invalidation plus acknowledged retained cleanup; C06 same-cell typed accepted config/revision publication before notification; C07 cancellation/wake before joins and inert registration; C08/C19 additive current-main integration; C15 pre-send expiry versus uncertain issued effects; C18 native callbacks enqueue rather than reenter policy. These are requirements, not certified native behavior.

## Source evidence

- Feature: [`_legacy/docs/features/hold-to-quit.md`](../../../_legacy/docs/features/hold-to-quit.md).
- Runtime, in `_legacy/app/native/src/modules/cmd_q/mod.rs`:
  - Intercepts keycode 12 with Command only (`:271-289`) and swallows both down and up (`:291-302`).
  - Ignores repeats (`:307-316`).
  - Polls every 100/16 ms (`:30-34`, `:174-209`).
  - Calls `terminate`, then `forceTerminate` (`:408-421`).
  - Has an unused hint event (`:426-439`).
  - Pause and resume only toggle the tap (`:481-501`).
  - Uses `unsafe impl Send/Sync` (`:112-119`).
  - Never re-enables a tap that `DisabledByTimeout`/`UserInput` turned off; there is no handler.
- Config:
  - `_legacy/app/native/src/config/types/command_quit.rs:16-35`: defaults `false`/`1500`, no validation.
  - `config/types/root.rs:83-88` and `_legacy/stache.schema.json:22-29`: wrongly say “Enabled by default”.
  - Schema `:467-484`: `holdDuration` minimum 0.
  - Template: `config/template.rs:46-56`.
- Event `stache://cmd-q/alert`: `events.rs:81-87`. Nothing in the UI subscribes to it.
- Tray: legacy `modules/tray/mod.rs:79-106` toggles pause and resume through `LifecycleRegistry`. Next tray: [`tray.rs`](../../../src/crates/app/src/tray.rs) (config-backed Enable, session Pause).
- Next [`migrate.rs`](../../../src/crates/cli/src/migrate.rs) `UNPORTED` (`:21-29`) copies `commandQuit` verbatim today.
- Current adapter source (`git show main:src/crates/macos/src/applications.rs` at `ad34b46`) has:
  - exact `NSRunningApplication` identity through opaque `ApplicationId`;
  - `IssuancePermit` revalidation at the send;
  - `TerminateOutcome { Requested, NotRunning, Refused, Stale }`;
  - a 2 s `ACKNOWLEDGEMENT_BUDGET` and a 10 ms `CANCELLATION_QUANTUM`.

  `ApplicationObservation::terminate` / `ApplicationBackend::request_terminate` call `forceTerminate`, not graceful `terminate`; these remain compatible for noTunes. No frontmost query or workspace-activation observation ships there yet; part 03 adds them and a separate graceful operation. Timeout abandons an unissued request; already issued effects are uncertain. There is no late-terminate completion callback to assume exists.

- `git show main:src/crates/cli/src/migrate.rs`: `plan` invokes `ConfigCatalog::rewrite_legacy` before typed inspection and emits absent registered templates. `commandQuit` stays in `UNPORTED` until part 01.
- `git show main:src/crates/sdk/src/registry.rs`: `reload_inner` calls `reconfigure` before assigning config/revision; `SlotState::drop` publishes metadata afterward. A typed accepted-config watch is absent at this baseline. A local `ApplicationObservation::accept` token is not the registry's published revision; part 04 owns its publication integration gate.

## Intentional compatibility changes

1. No `forceTerminate` fallback. A refusal is reported, not escalated.
2. Finder and Stache frontmost pass ⌘Q through.
3. The instance captured at down is terminated, not whatever is frontmost at the deadline. A frontmost change cancels the press.
4. The HUD shows at down, replacing the unused hint event. `stache://cmd-q/alert` is not ported.
5. The tap is re-enabled automatically, and an interruption cancels the in-flight press.
6. Pause, disable, stop and reconfigure clear the in-flight press. No stale quit happens on resume.
7. Lifecycle is generic SDK behavior:
   - config-backed Enable;
   - session Pause/Resume;
   - Accessibility waiting and recovery without a restart;
   - live reload, where legacy read the config once at init.
8. The docs and schema say disabled by default, which matches the legacy struct.
9. There is no 16 ms polling. The deadline is one owned wait and the HUD animates natively.

## Open questions

None. Duration range, physical key matching, no endpoint and required HUD are settled above. Public-API compile proofs and manual display/Space checks remain implementation gates, not unresolved product choices.

## Validation and safety

Panic containment means preventing unwinding across FFI in unwind-enabled builds. The retained release profile uses `panic = "abort"`: it cannot recover arbitrary Rust panics. Production callbacks must therefore be bounded and nonpanicking; fake unwind tests do not establish release panic recovery.

Default tests use fakes for input, applications and HUD. A real-device check (tap, HUD, quitting a disposable app such as TextEdit with no unsaved document) needs explicit approval for each run, with the exact steps listed in part 04. Docs-only planning ran no build or native code.

## Affected docs

- The [planned feature notice](../../features/hold-to-quit.md) stays marked NOT SHIPPED until part 04.
- The [index](../../index.md) gets an additive route.
- Part 01 updates [config](../../architecture/config.md).
- Part 02 updates [overview](../../architecture/overview.md) (the macOS adapter list).
- Part 03 updates the shipped applications adapter description without changing noTunes' force behavior.
- Part 04 writes the feature doc and narrowly updates [testing](../../guides/testing.md), [new-module](../../guides/new-module.md), [SDK](../../architecture/sdk.md) if its publication contract changes, and the landed roadmap/index additively.
- Legacy docs stay untouched.
