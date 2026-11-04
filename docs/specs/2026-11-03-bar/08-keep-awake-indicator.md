---
status: draft
---

# 08 — Keep Awake provider and authorized session control

Read when integrating the indicator after 07, **after Keep Awake lands**, and after separate approval. The [Keep Awake spec](../2026-11-03-keep-awake/00-overview.md), source commit `2ddd0e3`, is a draft reference, not an available module API.

## Scope

Extend the landed Keep Awake module's authoritative status publisher/DTO codec, SDK schema registration via existing app factory context, app `host.rs`/`bindings.rs`/capability policy and UI KeepAwake hooks/tests. Rebase first; no imports of `stache-keep-awake` in `stache-bar`, no power assertions or lock probing in Bar, no duplicate Rust provider DTO.

## Requirements

- Keep Awake becomes the first production provider of 02's typed published-state contract, schema identity owned by Keep Awake. Its original Rust DTO/codec is registered by the app (allowed to depend on both modules), serialized into the SDK envelope, validated by that codec and projected/exported by the same tauri-specta collector. Bar declares optional provider ID/schema capability and consumes the app's generated typed UI contract; generic SDK carries no module type knowledge. Do not copy Keep Awake Rust types into SDK/Bar or decode an unchecked JSON bag. Compile-proof this cross-crate wiring, including mismatched version rejection.
- Reuse Keep Awake's last-published runtime DTO and app-owned status envelope with held ConfigStore diagnostics (file-first, registry-second). Preserve C01 lock semantics: valid session dictionary/key absent means compatibility-unlocked; null/unreadable/present nonboolean unknown. Bar never probes or substitutes lock state. Keep Awake owns inactive/acquiring/active/releasePending and retained failed-release IDs; Bar does not claim cleanup/inactivity from lifecycle alone.
- Project exactly two legacy visuals: green filled Coffee02Icon only for current, available, Running/accepted generation with **confirmed adopted active** assertion; otherwise legacy unfilled/inactive, including locked, paused, disabled, absent, failed, unknown, acquiring and releasePending. Initial unresolved keeps legacy null loading branch. No third uncertain/error/pending color, badge, tooltip or disabled opacity/cursor. Runtime diagnostic may remain truthful outside Bar's visuals. Optional provider loss tombstones before wake and Bar remains Running; no `requires`/unknown-service registration failure.
- Click is generic host session Pause/Resume only, using existing module controls; **disabled/absent is inert**, never Enable, config edit or a new `keepAwake.toggle` endpoint. Choose pause/resume from current host session paused metadata, not green/lock/assertion color: Running-but-locked pauses; Paused resumes only if enabled. Failed/unavailable/releasePending ambiguous action returns typed non-success/busy/not-attempted rather than inventing a new acquisition or unsafe retry. Host serializes/rechecks current eligibility at execution; duplicate/stale clicks cannot invert a newer user decision. Rejected file diagnostics do not replace last accepted enabled state.
- Add minimal generic generated session-action command/result only if not already landed. Real caller label/owner/surface authorization limits Bar capability to Pause/Resume of `keepAwake`; reject all other modules/actions/forged label, Enable/Retry/config access. No arbitrary module control power just because endpoint is generic. Queue acknowledgment is not success: return actual applied/not-applied/stale/disabled/unavailable/failure or uncertain outcome in concrete Rust serde/Specta command data, never JSON-string errors. Read/write queues bounded and shutdown wakes waiters before joins.
- Subscribe-before-fetch/read, version/generation/sequence validation and snapshot recovery prevent stale initial data/late old publisher/tombstone races. Published provider revision matches accepted config, never staged/guessed value; exact wire counters stay strings. State payload is coherent; app lifecycle/runtime/diagnostics separately sampled are explicitly observational, not an invented globally atomic envelope. No native or disk work in reads.

## Acceptance criteria

- [ ] BAR-08-01: Landed Keep Awake owns first real typed provider/codec; app projection/generation/revision/sequence and optional unavailability compile without module-to-module dependency or duplicated DTOs.
- [ ] BAR-08-02: Legacy two-state visuals/locked mapping and initial-null state pass parity for all assertion/lifecycle/provider cases; optional absence never stops Bar.
- [ ] BAR-08-03: Authorized generic session Pause/Resume uses current paused/enabled metadata, disabled click is inert, no config/enable/new Keep Awake mutation endpoint, and concrete outcomes survive transport/races.
- [ ] BAR-08-04: Stale fetch/event/provider/stop/cleanup and diagnostic repair tests pass; feature/SDK/UI/testing/index docs reflect only actually shipped behavior and all gates remain honest.

## Tests

Fake first-provider registration and independent consumer; schema mismatch/duplicate publisher, absent optional provider, provider disabled/pause/fail/recover, acquiring/active/locked/releasePending/cleanup failure, key-absent unlocked versus failed probe, late old writes/tombstones and sequence reset on new generation. Never acquire real assertions or lock/unlock display. Barrier native acquisition/release while reads return promptly; diagnostic malformed JSON/wrong bool/deletion and identical-good repair through normal watcher retain accepted runtime/revision. UI initial fetch versus newer notification, overflow resnapshot and late listen cleanup. Fixed action tests Running unlocked/locked, Paused enabled, Disabled inert/zero config writes, absent/failed/busy, repeated stale clicks, external tray action races, authorization of every invalid module/action/route and shutdown uncertainty. Round-trip actual generated command serializer and app integration, not just mocked click handler. Assert no new CLI grammar and existing modules unaffected.

## Notes

Keep Awake's standalone policy, power lease correctness and manual power gates remain owned by its spec; this part cannot repair them by creating a second controller. Manual validation requires per-run permission and is not a default build/test side effect.
