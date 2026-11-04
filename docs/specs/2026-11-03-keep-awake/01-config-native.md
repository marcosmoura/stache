---
status: done
---

# 01 — Configuration and native feasibility

Read when implementing the first Keep Awake part, after explicit approval. This part ships no automatically registered runtime.

## Scope

Create the authoritative config crate and narrow fake-injectable native adapters. Prove the public binding/thread/ownership path before part 02. This part must build and test alone, with no production assertion acquisition.

Known integration points:

- Root `Cargo.toml`/`Cargo.lock`; new `src/crates/modules/keep-awake/{Cargo.toml,src/lib.rs,src/config.rs}`.
- `src/crates/app/src/catalog.rs`, `bindings.rs`, and existing schema/bindings generators; generated `schemas/keepAwake.schema.json` and TypeScript output.
- `src/crates/cli/src/migrate.rs` and its tests; CLI remains module-independent and obtains defaults through the injected catalog.
- `src/crates/macos/{Cargo.toml,src/lib.rs}` and narrow assertion/lock adapter files (suggested `src/keep_awake.rs`); add only binding features/dependencies required by proven APIs.

## Requirements

### Configuration

1. Export `KeepAwakeConfig` with public `enabled: bool`, `Default` false, camelCase serde, schemars descriptions, Specta `Type`, standard equality/clone traits and `ModuleConfig`. Use `KeepAwakeConfig::ID = ModuleId::new("keepAwake")` everywhere; no second template/default definition.
2. Register the type in the shared app catalog and explicit bindings collection. `config init`, `config path`, `schema keepAwake` and generators discover it through existing mechanisms; these are extensions of existing commands, not a Keep Awake CLI.
3. Missing file/`{}` gives disabled defaults. Malformed, empty or mistyped initial files remain invalid with diagnostics, never silently defaulted. Unknown keys retain the generic warning policy; no extra settings are recognized.
4. Migration always plans the authoritative disabled `keepAwake.jsonc` template, whether legacy Bar is enabled, disabled or absent. Legacy has no Keep Awake setting; do not create or infer a separate migration flag. Preserve source/collision/force/atomic-write rules and unrelated unported-section copies; report the new disabled-default output clearly.
5. Do not add a production factory, start a module from a generator, acquire a lease while building a catalog, or expose an enabled tray runtime in this part. Catalog-only files are not reported as bound/active.

### Safe native boundary

6. Expose a narrow safe injectable assertion backend/owned lease plus lock probe/observation contract from `stache-macos`. A complete lease means all requested display, idle and system assertions were created; retain the fixed macOS reason `Stache requested system wake lock`. Explicit release and partial-acquisition rollback must return observable outcomes and retain every unsuccessfully released ID in a generation-safe module/native-backend owner until resolved or the explicit process-exit boundary. Unmodified `keepawake 0.6` Drop ignores release errors and is insufficient; use a minimal appropriate/direct IOPM binding wrapper in `stache-macos` if needed, not module unsafe or a new framework. Check documented release-error meanings before treating an ID as proven inactive; unknown failure is never success. Cleanup is idempotent for confirmed-released IDs, with exactly one successful release per ID; failed attempts may be retried. Fallback Drop is best-effort only: it cannot claim success or silently discard pending ownership. No fresh acquisition while prior IDs remain unresolved; use bounded owned reconciliation/next lifecycle cleanup, not busy loops, a broad reaper or a permanent detached cleanup thread.
7. Lock results distinguish locked, unlocked and unknown with a typed reason. A valid dictionary with the key absent maps to unlocked for approved best-effort legacy compatibility; a present CFBoolean true/false maps to locked/unlocked. Null/unreadable dictionary or a present non-boolean value maps to unknown and suppresses acquisition. Distinguish absence from query failure, validate type before reading a CFBoolean and honor Create/Copy ownership on every path. This is no public lock-state guarantee. Keep raw pointers/Objective-C/unsafe within `stache-macos`.
8. Subscribe to the legacy distributed notification names as refresh hints only; a callback does not assert authoritative lock state. Use an owned observer registration, no static `OnceLock`, leaked selector object or detached run-loop thread. Contain callback panics before crossing native boundaries; callbacks signal/coalesce and return, without acquisition, probing or lifecycle calls.
9. Establish main-run-loop ownership against installed bindings. `Module` requires `Send`; the existing `ActivationObserver` is main-owned and `!Send`. Do not mark raw pointers/retained observer guards `Send` or move them into a module. A narrow app-owned main-thread owner/dispatcher is allowed if necessary; keep native objects/removal on their proven owner thread and retain only safe generation-scoped handles elsewhere. Do not invent a reusable service framework or depend on noTunes work.
10. Install/remove dispatch acknowledgements have documented finite deadlines and cancellation. All off-main lifecycle waits must terminate if the main queue never drains. Main never waits on a worker waiting for main. Cancelled/timed-out installation cannot later create a live observer; a queued install rechecks identity/cancellation, and a racing completed install removes only its own registration. Removal invalidates callbacks immediately; if native cleanup must await a stalled main queue, retain safe owner-local cleanup state, report timeout truthfully and document eventual removal instead of freeing a still-registered observer.
11. Observation failure is typed and permits the runtime's two-second probe-only fallback. A non-delivering notification service is not detectably healthy merely because registration returned; polling remains required. Part 01 defines the adapter, not a background timer that starts on construction.

### Required gate before runtime

12. Compile-prove actual thread traits, creation/removal calls, retained lifetimes and dispatcher cancellation with the installed objc2 Foundation/CoreGraphics interfaces. Record exact dependency/features and a small ownership diagram in adapter rustdoc; build proof must not execute real assertion calls or doctest examples that acquire them.
13. Use the public `CGSessionCopyCurrentDictionary` function through suitable bindings, not private symbols. Explicitly document that `CGSSessionScreenIsLocked` and legacy notification names are best-effort compatibility evidence, absent a documented guarantee. Do not equate NSWorkspace session-active, console presence or login completion with unlocked. If no safe approved approach is feasible, block part 02 and ask rather than silently changing semantics.
14. Test dispatcher deadlines/queued install-remove races with fake scheduling; identify any remaining real-run-loop observations as manual/unverified. Add no real native smoke to the default test suite.

## Acceptance criteria

- [x] KA01-01: Config defaults, serde casing/type rejection, `ModuleConfig`, schema and Specta export all derive from one type; generated checks are deterministic.
- [x] KA01-02: Catalog commands include `keepAwake`; migration with legacy `bar.enabled: true`, false and absent emits disabled defaults and preserves existing overwrite/source-safety behavior.
- [x] KA01-03: No production runtime/factory, assertion, observer or webview is started by config, migration, schema/bindings generation or default tests.
- [x] KA01-04: Fake creation failure at each position rolls back preceding IDs; release/rollback failure at each ID retains pending ownership, blocks fresh acquisition and supports bounded recovery. Fake barriers allow part 02 to observe creation and release/rollback in progress before native completion. Each ID is successfully released exactly once; repeated confirmed cleanup is inert and Drop attempts are not proof of release.
- [x] KA01-05: Probe maps valid booleans and valid-dictionary/key-absent compatibility correctly; null/unreadable dictionary and present wrong-type data produce typed unknown without invalid reads or ownership leaks.
- [x] KA01-06: Compile-proven safe ownership satisfies `Module: Send` without unsafe Send shims; native callbacks contain panics and only signal.
- [x] KA01-07: Fake dispatcher proves bounded cancellable installation/removal, late-install suppression, callback invalidation and successor identity safety; timeout cleanup limitations are documented.
- [x] KA01-08: Public/best-effort API evidence and gate result are recorded; all listed checks pass without power operations, and lasting docs describe config-only availability honestly.

## Tests

- Config/catalog/template/schema goldens: missing/empty-object false, explicit true/false, invalid boolean/empty text, unknown-key warning, exact file/schema IDs and no duplicate registration.
- Migration: legacy Bar enabled/disabled/absent, unrelated sections unchanged, disabled-default report, existing destination refusal/force and source never overwritten. CLI tests use the injected catalog, not a module import.
- Native fakes: creation failure at each of three positions; explicit release failure at each ID; partial rollback failure, pending-ID retention, bounded retry/recovery, correct IDs, exactly-once successful release and predecessor cleanup never releasing a successor. Provide barriers for in-flight native creation and release/rollback so runtime tests can check pre-call status publication without real assertions. Distinguish best-effort Drop evidence from confirmed release.
- Probe fakes: valid dictionary/key absent → unlocked; CFBoolean true → locked, false → unlocked; null/unreadable dictionary or present non-boolean → typed unknown. Verify type/Copy ownership paths, observer unavailable, callback panic containment and coalesced signals.
- Dispatcher fakes: no main drain, cancellation before/after install, timeout then late queue drain, remove twice, callback after invalidation, predecessor cleanup cannot remove a successor. No real power calls in tests or executable rustdoc.

## Validation and docs

After implementation, run from this worktree (not now during planning):

```sh
cargo nextest run -p stache-keep-awake -p stache-macos -p stache-cli -p stache-sdk
cargo nextest run -p stache-app --features dev-tools
bun run lint
bun run format:check
bun run test
bun run check:generated
bun run build
bun run tauri:build
RUSTDOCFLAGS='-D warnings' cargo doc -p stache-keep-awake -p stache-macos -p stache-app --no-deps
```

Regenerate schemas/bindings through existing `bun run generate-schema`/`bun run generate-bindings` only when implementing. Update [config](../../architecture/config.md) for the new file/schema and disabled migration rule; retain a clear config-only/not-runtime notice in [feature docs](../../features/keep-awake.md). Link verification and formatting are required. Real native verification needs fresh explicit authorization as described in the [overview](00-overview.md#validation-and-safety).
