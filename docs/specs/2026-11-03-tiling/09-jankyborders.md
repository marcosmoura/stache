---
status: done
---

# 09 — Optional external JankyBorders integration

> **Superseded** by [Tiling native borders](../2026-11-09-tiling-borders/00-overview.md) (part 08, 2026-11-11): the JankyBorders path described here was removed; Stache draws borders itself. Kept as history.

Read when implementing after 08 and separate approval. External integration is retained, not silently replaced with a custom border engine.

## Scope

Tiling `src/borders/{policy.rs,runtime.rs}`; `stache-macos` safe Mach/CLI borders adapter and fakes, exports/framework wiring (no Mach code exists in `stache-macos` yet; this part adds it). Config is landed: `config/borders.rs` (`BordersConfig`, `BorderStyle`, `BorderAnimationConfig`) and `config/color.rs`. Legacy `modules/tiling/borders.rs` and `config/types/{borders,color}.rs` are parity evidence only. The CLI helper reuses `CommandExecutor` (`sdk/src/commands.rs`: `run_captured`, `RetainedChild`, `resolve_binary`, `CaptureLimits::new`, `CANCEL_CLEANUP` 2 s, `MAX_CAPTURE_BYTES` 1 MiB); no parallel process runner. All native unsafe stays macOS-only. No Bar dependency.

## Requirements

- Preserve service `git.felix.borders`, NUL-separated argv protocol, batched width/style round|square/hidpi/active-inactive settings and CLI fallback. Port exact solid/gradient/glow conversion, angle boundaries, focus/floating/layout override precedence and blacklist (tiling ignore + border ignore + system skip names/IDs). Disabled/missing layout override falls back to focused per legacy `focused_state_config`; no accidental reinterpretation.
- **Compile gate first:** verify Mach message header/body/OOL descriptor size/alignment, port rights ownership/deallocation, buffer lifetimes, timeout flags/bounds, real framework linkage and safe owner confinement. Tests fake transport; compile success does not prove JankyBorders version/protocol compatibility. Approved manual protocol test records external binary/version, service identity, expected argv/settings and actual acknowledgement limitations.
- Owning a send right/connection does not own the service process/global settings. Resolve external executable safely. Legacy only invokes the external `borders` binary (`borders.rs:519–527`); running it without a live service may make that invocation become the service (external behavior, unverified), so fallback must preflight authority/service existence and cannot silently launch it. Never launch/kill/restart a foreign borders service or alter its settings without explicit consent for that run/configuration. No new settings as an ownership workaround.
- Before first mutation establish either provable exclusive runtime-owned service/settings authority or explicit user consent naming affected shared settings and non-restorable limitations. Do not assume existing global settings can be read/restored. **Decision (approved 2026-11-08):** a fixed Mach service name cannot prove exclusive ownership, so authority is consent: accepted `tiling.borders.enabled: true` is the explicit opt-in. The `enabled` schema description and feature docs name the shared JankyBorders settings Stache overwrites (width, style, hidpi, active/inactive colors, blacklist) and state that their previous values are not read or restored. No new config field. Borders disabled (default) sends nothing.
- **Decision (approved 2026-11-08): never launch, leave on stop.** Stache never starts, restarts or kills a borders service. Each attempt first looks up `git.felix.borders`; if absent, publish a missing-service diagnostic and send nothing, including no CLI fallback. The CLI fallback runs only after a successful lookup in the same attempt; the residual race where the service exits between lookup and CLI invocation is documented, not claimed impossible. Stop/pause/disable/reload-to-disabled cancels own animation and releases the owned connection and sends **no** mutation (no `width=0`/transparent pause); the service keeps the last settings Stache sent, and docs/status say so.
- One owned bounded latest-update runner; dedupe argv by argument boundaries. New base command bumps monotonic epoch; actual animation send rechecks epoch under narrow send ordering so stale8fps frames cannot overwrite focus. Maximum8fps, cancellation-wakeable pacing; no lock held across sleeps. Failed send not cached as success; next **new** update/explicit retry may retry, no unbounded autonomous loops or fallback after ambiguous already-issued send.
- Planned engineering bounds, not user config: one in-flight Mach send with a ≤0.5 s per-message deadline (a deliberate bound; legacy frame sends use a 0 ms Mach timeout, `borders.rs:293`, and commands 100 ms, `:292`), helper CLI child lifetime ≤2 s, another 2 s for cancel/reap before retaining an owned handle, and captured stdout/stderr each capped at 64 KiB (within `MAX_CAPTURE_BYTES`). Cancellation-wakeable sleeps; no duration timer exists while borders are disabled. Bound Mach and CLI duration/output, cancel/reap only the exact helper child this runtime spawned (not the service); no shell interpolation. Separate known transport rejection from uncertain delivery. Failure degrades borders only, leaving layout/commands/other modules running; expose missing service/authority/timeout/protocol/cleanup diagnostics. Disabled borders allocate no runner/port/child.

## Acceptance criteria

- [x] TI-09-01: Compile-/fake-proven Mach protocol/rights/buffer/timeout handling and authority-aware CLI fallback preserve exact settings/color/blacklist/style behavior.
- [x] TI-09-02: Connection versus service/settings authority is explicit; `borders.enabled: true` is the only consent, an absent service gets a diagnostic and no launch/CLI fallback, and no service ever receives a launch/kill/restart or stop-time pause/`width=0` command.
- [x] TI-09-03: Dedupe/epochs/8fps/pacing/cancellation and typed failed/uncertain delivery remain bounded without stale focus overwrite or retry loops.
- [x] TI-09-04: Optional failures/cleanup retain exact owned resources and truthful diagnostics, leaving core/other modules unaffected; native protocol/ownership evidence is recorded or marked blocked.

## Tests, rollback and docs

Fake Mach encoder, wrong descriptor/port lifecycle, timeout/rejection/unknown delivery, service absent/foreign/consented/exclusive, malicious argv, child hang, same concatenation/different argument boundaries, focus/animation races and shutdown blocked send. No default service lookup/send or CLI process. Manual integration only per [validation](validation.md), never manage user's existing service silently.

Rollback stops own runner/releases rights/reaps own helper; does not reset foreign settings to defaults. Update feature/native SDK/testing/performance docs and explicit external non-restorability limitations.
