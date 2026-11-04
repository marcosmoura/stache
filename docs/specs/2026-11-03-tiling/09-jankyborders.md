---
status: draft
---

# 09 — Optional external JankyBorders integration

Read when implementing after 08 and separate approval. External integration is retained, not silently replaced with a custom border engine.

## Scope

Tiling `src/borders/{policy.rs,runtime.rs}`; `stache-macos` safe Mach/CLI borders adapter and fakes, exports/framework wiring. Legacy source `modules/tiling/borders.rs` and `config/types/{borders,color}.rs`; all native unsafe stays macOS-only. No Bar dependency.

## Requirements

- Preserve service `git.felix.borders`, NUL-separated argv protocol, batched width/style round|square/hidpi/active-inactive settings and CLI fallback. Port exact solid/gradient/glow conversion, angle boundaries, focus/floating/layout override precedence and blacklist (tiling ignore + border ignore + system skip names/IDs). Disabled/missing layout override falls back to focused per legacy `focused_state_config`; no accidental reinterpretation.
- **Compile gate first:** verify Mach message header/body/OOL descriptor size/alignment, port rights ownership/deallocation, buffer lifetimes, timeout flags/bounds, real framework linkage and safe owner confinement. Tests fake transport; compile success does not prove JankyBorders version/protocol compatibility. Approved manual protocol test records external binary/version, service identity, expected argv/settings and actual acknowledgement limitations.
- Owning a send right/connection does not own the service process/global settings. Resolve external executable safely; CLI may auto-start service in legacy behavior, so fallback must preflight authority/service existence and cannot silently launch it. Never launch/kill/restart a foreign borders service or alter its settings without explicit consent for that run/configuration. No new settings as an ownership workaround.
- Before first mutation establish either provable exclusive runtime-owned service/settings authority or explicit user consent naming affected shared settings and non-restorable limitations. Do not assume existing global settings can be read/restored. If neither is possible, stop for decision and keep this part blocked; do not claim integration removed or safely managed. No blanket `width=0`/transparent pause on a foreign service. Stop cancels own animation/releases owned connection; any cleanup mutation needs the same authority and a documented restoration plan, otherwise report remaining external settings honestly.
- One owned bounded latest-update runner; dedupe argv by argument boundaries. New base command bumps monotonic epoch; actual animation send rechecks epoch under narrow send ordering so stale8fps frames cannot overwrite focus. Maximum8fps, cancellation-wakeable pacing; no lock held across sleeps. Failed send not cached as success; next **new** update/explicit retry may retry, no unbounded autonomous loops or fallback after ambiguous already-issued send.
- Planned engineering bounds, not user config: one in-flight Mach send with a ≤0.5 s per-message deadline, helper CLI child lifetime ≤2 s, another 2 s for cancel/reap before retaining an owned handle, and captured stdout/stderr each capped at 64 KiB. Cancellation-wakeable sleeps; no duration timer exists while borders are disabled. Bound Mach and CLI duration/output, cancel/reap only the exact helper child this runtime spawned (not the service); no shell interpolation. Separate known transport rejection from uncertain delivery. Failure degrades borders only, leaving layout/commands/other modules running; expose missing service/authority/timeout/protocol/cleanup diagnostics. Disabled borders allocate no runner/port/child.

## Acceptance criteria

- [ ] TI-09-01: Compile-/fake-proven Mach protocol/rights/buffer/timeout handling and authority-aware CLI fallback preserve exact settings/color/blacklist/style behavior.
- [ ] TI-09-02: Connection versus service/settings authority is explicit; foreign instances receive no unconsented launch/kill/reconfigure/pause command and unresolved management blocks acceptance.
- [ ] TI-09-03: Dedupe/epochs/8fps/pacing/cancellation and typed failed/uncertain delivery remain bounded without stale focus overwrite or retry loops.
- [ ] TI-09-04: Optional failures/cleanup retain exact owned resources and truthful diagnostics, leaving core/other modules unaffected; native protocol/ownership evidence is recorded or marked blocked.

## Tests, rollback and docs

Fake Mach encoder, wrong descriptor/port lifecycle, timeout/rejection/unknown delivery, service absent/foreign/consented/exclusive, malicious argv, child hang, same concatenation/different argument boundaries, focus/animation races and shutdown blocked send. No default service lookup/send or CLI process. Manual integration only per [validation](validation.md), never manage user's existing service silently.

Rollback stops own runner/releases rights/reaps own helper; does not reset foreign settings to defaults. Update feature/native SDK/testing/performance docs and explicit external non-restorability limitations.
