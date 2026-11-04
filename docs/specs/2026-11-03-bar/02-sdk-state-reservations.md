---
status: draft
---

# 02 — SDK published state and screen reservations

Read when implementing generic fake-backed contracts after 01 and separate approval. No native panel or Keep Awake provider yet.

## Scope

Extend `sdk/src/{context,module,dependencies,registry,status,lib}.rs`; suggested `published_state.rs`, `reservations.rs` with colocated tests. Pass capabilities through the one app factory context in `app/src/host.rs`, reusing landed context/accepted-config watch rather than introducing a second framework. SDK remains Tauri/native/module independent.

## Requirements

### Published state

- Host owns an authoritative read-only latest-state cell plus notification. Provider registers one typed schema contract, identified by provider ID + schema identity/version; payload is serde-serializable typed Rust at publication, not arbitrary caller-selected JSON masquerading as another provider. Validate duplicate IDs, mismatched type/schema/version and unauthorized publishers deterministically. App registers schemas from owning modules; SDK never imports them.
- Add minimal generic `observes` declaration/capability separate from hard lifecycle `requires`. Unknown optional provider is a valid registration and reads explicit unavailable/absent; disabled/stopped/failed providers read unavailable with reason, never stale active data. Missing hard `requires` retains existing failure behavior. Consumers cannot mutate observed state. Keep Bar Running when optional provider stops.
- Envelope carries exact runtime owner/generation, accepted config revision, monotonic state sequence and schema identity. Exact u64 wire values become opaque decimal strings at app boundary. Revision and payload must describe the same accepted generation/config: staged reconfigure cannot publish a guessed revision + 1. Reuse accepted-config watch; prove publication-after-acceptance with barriers. Payload-internal fields are one atomic snapshot; independently sampled host diagnostics are explicitly not globally atomic.
- Publish cell before wakeup; bounded/coalesced notification is only a hint to reread latest. Race-free check/wait, cancellation/stop wake, no lost wake between read and sleep; no EventBus-as-state or polling forever. Subscribers never hold registry/module locks or execute provider callbacks. Bounded cancellation quantum and tests, no unbounded queue per sample.
- Generation-scoped publisher invalidates immediately on stop/pause/disable/failure; reject late writes, publish tombstone/unavailable and wake readers before cleanup waits. Retained old handles cannot clear/update successor state. New generation identity distinguishes sequence reset. Last known debug/cleanup facts may remain explicitly historical, not available active state.
- Compile-prove independent fake producer and consumer crates/modules, wrong-type usage and app schema registration. No Bar↔Keep Awake crate dependency; part 08 supplies the first real provider. Schema contract carries serialized payload validated/deserialized with the provider's app-registered codec; app imports the original DTO for generated bindings/projection, not duplicate Rust structs in Bar.

### Host-owned reservations

- Generic typed `DisplayId` comes from native enumeration/projection, not Bar label, array index or guessed primary UUID. SDK stores no native pointer. Contract defines lifetime/reuse/removal; current topology generation disambiguates transient numeric-ID reuse. Part 03 implements enumeration and hotplug.
- Owner/generation capability stages validated logical edge insets per display and revision; finite, nonnegative, checked arithmetic. Replace atomically, idempotent remove, coherent read-only snapshot. Aggregate same-edge requests by maximum, never sum overlapping reservations unless a future explicit nonoverlap contract exists; deterministic ordering and all four edges. No config cross-reading or Tiling implementation.
- Stop/pause/disable/failure immediately removes exact-generation logical reservations even if native cleanup remains pending. Old release cannot remove successor; failed publication/partial start rolls back exact owner. Display removal removes orphan entries. Later Tiling consumes the registry, not `bar.jsonc`.
- Bar's eventual reservation is top `height + padding` on primary only, constant while sliding away. Native creation acknowledgments and logical reservations are distinct: a pending/failed panel is not an actual loaded surface. Part 03 publishes actual states and clears staged reservations on failure; accepted config alone does not imply a running owner or successful panel.

## Acceptance criteria

- [ ] BAR-02-01: Typed schema/identity/authorization and optional-observation contract compile with independent fake consumers and preserve hard requires behavior.
- [ ] BAR-02-02: Atomic accepted-revision/state envelopes, publish-before-wake, bounded cancellation and tombstones reject stale generations under adversarial scheduling.
- [ ] BAR-02-03: Reservation validation, deterministic max aggregation, revision/topology identity and exact-owner cleanup pass without native work or config reads.
- [ ] BAR-02-04: One factory context preserves current modules; SDK/new-module docs and compile-proof examples describe only landed contracts.

## Tests

Fake scheduler/barriers cover absent/disabled/paused/provider recovery/schema mismatch, slow consumer/notification overflow and resnapshot, publication during check/sleep, A–B–A config/revision transitions, rejected reload, accepted config followed by start failure, stalled provider with prompt published reads, cancel during wait, late old publish/tombstone after successor. Reservation tests: two modules on same/different edges/displays, replacement/removal ordering, failed start, stop/disable/pause, display removal/ID reuse and invalid/nonfinite/overflow data. Existing registry/window/factory suites remain unchanged in behavior. No native adapters in tests.

## Notes

Concrete signatures are implementation choices, but must compile in this part. These are narrow state/reservation capabilities, not general service discovery, lifecycle callbacks or a command bus.
