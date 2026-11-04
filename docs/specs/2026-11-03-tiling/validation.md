# Planned validation and native safety gates

Read when approving/validating any implementation part. **Draft; no tests, builds or native effects are authorized by this planning task.** Criteria belong to their numbered parts, not this appendix.

## Every part

Rebase current main; confirm all preceding rewrites landed and foundations exercised. Review actual diff against [inventory](inventory.md), retain additive other-module registrations/docs/generated collections and run targeted fake tests. Compile API consumers at introduction, not a later integration part. Full relevant future gates:

```sh
cargo check --workspace --all-targets --features stache-app/dev-tools
cargo nextest run --workspace --features stache-app/dev-tools
bun run lint
bun run format:check
bun run test
bun run test:ui:coverage
bun run generate-schema -- --check
bun run generate-bindings -- --check
bun run build
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
```

Compile-only packaging via `bun run tauri:build` is separate from launching. Default tests/doctests never query real AX, install taps, move/resize/hide/focus/launch/kill apps, change keyboard/audio/wallpaper/power/permissions, contact JankyBorders or use real user config. Use injected fakes/clocks/temporary paths. No legacy install/build/test/automation; inspect frozen source fixtures only.

## Deterministic safety evidence

Barrier tests at acquisition, accepted-watch publication, admission, exact identity preflight, actual native issuance, per-component acknowledgement, animation retarget, seal, restore and cleanup. Hold a fake native send until the test releases it: logical invalidation still completes immediately, queued actions become inert, timeout is truthful and cleanup ownership retained. Fake barriers do not assert a real OS call can be preempted. Cover A–B–A revisions, reused same-app CG IDs/PIDs, two registries, generation turnover, empty/malformed data, partial writes and failed restores. Require cancellation-wakeable waits and zero locks held across module callback/native dispatch/join except the narrow immediate-send guard.

Every feature loss has one observable reason/counter, exact resource ownership and recovery path; Running is not "all effects applied". Shutdown begins cancellation before IPC joins, keeps main dispatch alive through reverse-stop/finish, drains issued animations before restore and never clears quarantine to permit restart. Unsupported host Cmd-Q/logout teardown is an unrelated known gap, not a Tiling fix or accepted coverage claim.

## Per-run native approval

Each run requires explicit fresh permission naming test apps/exact instances, temporary Next config, intended actions/displays, permissions and cleanup. No blanket session approval; refusal or missing equipment leaves gate blocked/unverified. Never touch real user windows to "just check". Do not launch/kill by shared executable name, terminate unrelated processes or enable other modules. Permission prompts/changes require their own approved scope.

1. Part03: compile before approved read-only AX/enumeration/display/tab capability probe on named owned test app. Record framework/OS builds, owner thread, unknown attributes, public/private API need, identity and latency evidence. Confirm main executes no AX query/action and that a deliberately unresponsive owned test app cannot delay an unrelated main-queue probe (Bar/HUD stand-in). Read-only results are not effect feasibility proof.
2. Part06: separately approve controlled frame/focus/hide/unhide on owned test apps, including fixed-size/refusing windows, multiple windows of one app across workspaces/displays, external prehidden/show behavior and cleanup failure simulation. Report actual unapplied/partial frames, same-app isolation limits, exact owned restore and quarantine. No promise to restore all original geometry on stop; only runtime-hidden apps are restored.
3. Part08: separately approve tabs/drag/opt-in animation and passive input observation; validate HQ>MA>KB priority remains unchanged with Tiling. Record lost-input behavior, user-resize cooperation and stop-during-animation ordering; no synthetic keyboard/click injection by default.
4. Part09: separately approve versioned JankyBorders protocol/settings operation and ownership/consent. Owned connection is insufficient authority over a foreign service. Missing safe authority stops for decision, not width=0 cleanup. Record what cannot be restored; never silently launch/kill/reconfigure service.
5. Part10: separately approve Next Bar/display/hotplug/fullscreen/native Space observation and full supported tray Quit path. Virtual workspaces remain distinct from macOS Spaces. Verify reservations clear only on stop, not slide-hide; independent modules survive Tiling disable. Native Space/AX restrictions stay documented as observed limits, not promises from compile proof.

End each run with exact owned cleanup result/pending IDs and retained diagnostic evidence; restore only approved changes. No unsupported success claim when cleanup failed.

## Performance and no-fighting budget

Before active implementation acceptance, record release-host disabled baseline on identical hardware/OS/config/displays with warmup, fixed duration and repeated trials: tray-ready/start responsiveness, CPU, wakeups, memory, queue high-water/drops, AX calls, frame/observer latency and shutdown latency. Compare module-off/on attributable deltas for idle, event burst,100+ tracked windows, grid overflow, drag, animations off/on, borders off/on and Bar state updates. Disabled owns zero Tiling workers/timers/observers/taps/ports; optional absent resources stay zero. Passive shared-service ownership is counted separately from pre-existing consumers.

Deterministic bounds: semantic queue1024; bounded cardinality geometry/overflow; maximum10 grid frame targets; geometry flush no faster than configured30–360Hz; border frames<=8fps; one latest animation plan; no retry loop after refused/uncertain frame; finite cancellation wake quantum/hop acknowledgement. Coalescing must reduce redundant writes, not hide drops or change focus/create/destroy ordering.

Acceptance budget: no statistically distinguishable idle/per-operation regression beyond the measured run-to-run noise envelope against an independently approved comparable legacy sample, plus no sustained AX retry/fighting after app refusal or user drag. Do not invent numeric baseline from ~34.6k LOC. If no comparable approved legacy measurement exists, publish current module-off/on results and mark relative budget unverified; do not run legacy automation to obtain one. Any budget amendment needs approval. Publish raw samples/method and known missing native cases; compile/fakes/parity screenshots alone do not prove performance.

## Documentation-only checks now

Specification writing itself runs only a scoped Markdown formatter (existing direct binary, no install) plus local file/anchor links, whitespace, draft statuses, unique single-owner acceptance IDs and inventory coverage. It changes no generated files and runs no native tests or sibling writes; an approved docs-only milestone commit on the spec branch is separate from that check.
