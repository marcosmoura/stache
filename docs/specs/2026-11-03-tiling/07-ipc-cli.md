---
status: draft
---

# 07 — Complete IPC and CLI contracts

Read when implementing after 06 and separate approval. [CLI appendix](cli-contract.md) is normative and has no separate acceptance IDs.

## Scope

Tiling `src/{ipc.rs,cli.rs,dto.rs}` and colocated parser/render/router tests; app `commands.rs`, `host.rs`, `bindings.rs`/generator collection; SDK router/client only minimal additive changes if needed. CLI crate keeps SDK-only dependencies, module contribution is app-composed. Source mapping: inventory CLI/query/bridge rows.

## Requirements

- Contribute `tiling pause|resume|query|window|workspace` through existing `CommandCatalog` and one host socket Router. Preserve complete grammar, flags/conflicts/order/output in appendix. Selected global `--config DIR` remains a directory, validates host config identity; CLI never starts desktop, AX owner or module, including when host absent.
- Typed requests/DTOs decode once/encode once, use existing wire error kinds, never stringify errors into `Response::Ok` or extend wire error enums to hide module outcomes. Operation `Response::Ok` data carries module-owned tagged `Enqueued`/`Applied`/`Partial`/`Uncertain` plus request/target/component/failed/not-attempted/unknown evidence. No field called successful means actor accepted. Queries read one confirmed snapshot; malformed/decode/timeout never becomes empty success.
- Existing silent successful mutation output remains: exit0 means **Enqueued** acceptance (legacy contract), or confirmed Applied scope; docs must explicitly state it is not proof native layout applied. If immediate known failure/Partial/Uncertain is returned, exit1 with typed actionable adapter, do not silently exit0. Native asynchronous later results remain query/status diagnostics, not a retroactive CLI result. Do not wait for all animation/desktop writes to preserve acceptance semantics.
- Fixed multi-action order, each request waits for actor admission/state-transition ordering before next selector resolves. Focus intent determines subsequent selected window logically, without pretending native focus acknowledgement. Stop subsequent operations on first reported failure; already admitted/issued operations cannot roll back. Preserve resize pair order and late semantic-validation boundary (prior actions may already have been admitted).
- Query connection/remote/decode failures exit1, explicitly correcting legacy exit0. Retain legacy query stdout bodies/JSON error object grammar with serde escaping; no duplicate stderr line. Parse/arity/value-enum conflicts exit2; semantic validation/action failures exit1. Preserve existing adapter prefixes/bodies/streams through goldens. New uncertain/partial bodies are fixed in appendix, not ad hoc Debug strings.
- Pause waits for actual teardown acknowledgement; cleanup failure/uncertainty is not Applied. Resume uses registry semantics and cannot override disabled desired config or quarantine. Queries while unavailable return typed unavailable/disabled rather than fabricated empty state. Active query apps uses owner-published catalog, not live AX enumeration in IPC thread.
- Each request has one finite deadline within the shipped 5 s client total budget; multiple native hops share that remaining budget (individual cap 2 s), not N × 2 s before answering. Admission timeout abandons unissued work; issued actions remain uncertain. Host begin-shutdown cancels all handler waits before connection joins; no retry after possible send, no generic JSON command bus or public incomplete handler.

## Acceptance criteria

- [ ] TI-07-01: Full grammar/selector/filter/fixed action order and snapshot JSON/table/empty/ANSI contracts match appendix goldens through actual composed CLI and router/client serde.
- [ ] TI-07-02: Typed Response::Ok outcomes distinguish admission/application/partial/unknown; silent accepted success and every intentional exit/error correction are documented and tested.
- [ ] TI-07-03: Config identity, disabled/paused/quarantine/absence, malformed data and deadline/shutdown races never launch desktop, retry issued work or hang handler joins.
- [ ] TI-07-04: IPC/CLI/config/feature docs describe actual landed command guarantees and preserve prior contributions/completions/bindings.

## Tests, rollback and docs

Real temporary Unix socket/router/client + fake actor/native owner, injected stdout/stderr/ANSI and snapshot fixtures. Every command/action/filter alone and combined, empty and detailed JSON/tables, Unicode/escaping, unknown/stale ID, first/middle action failure, queue saturation, connection failure/timeout, shutdown blocked admission and native uncertainty. Goldens are extracted/reviewed from pinned source, never execute legacy. [Validation](validation.md) applies.

Rollback removes only contribution/handler after handler waits are cancelled and drained; no socket/schema/catalog overwrite. Update `architecture/ipc.md`, feature, CLI examples/testing and index. Later 08/09 runtime enhancements cannot silently change these contracts.
