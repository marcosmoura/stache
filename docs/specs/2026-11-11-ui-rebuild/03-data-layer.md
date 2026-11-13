---
status: done
---

# 03 — Data layer

## Scope

The typed IPC boundary, the query client, the generic ordered feed, the host and Bar queries, and the zustand scoped-store helper. Depends on part 01. No Rust change.

## Requirements

### IPC boundary (`src/ui/src/ipc/`)

- `ipc/` is the only directory that imports `@tauri-apps/*` or `bindings.gen.ts` (lint-enforced since part 01). It re-exports the generated DTO types and exposes `commands` and `events` unchanged, plus a small `subscribe(event, handler)` that returns a synchronous dispose function and unlistens immediately when `listen` resolves after disposal.
- Command results that are typed outcome unions stay unions. Nothing is converted to thrown errors except a rejected `invoke`.

### Query client (`src/ui/src/queries/`)

- `createQueryClient()` with defaults for a local IPC app: `networkMode: 'always'` for queries and mutations, `refetchOnWindowFocus: false`, `refetchOnReconnect: false`, `retry: false`, `staleTime: Infinity`. A query that fetches a remote service later overrides these in its own options.
- `QueryProvider` creates one client per window and joins the `App` provider composition.
- Every query is defined once as a `queryOptions` factory, grouped per domain (`queries/host.ts`, `queries/bar.ts`). Components call `useQuery` or `useSuspenseQuery` with the factory and a `select`; no component writes a query key by hand.

### Feed (`src/ui/src/queries/feed.ts`)

A feed keeps one query's data current from a host event. `createFeed` takes the query options, the snapshot command, the event, and how to read a sequence from a payload. Behavior:

- Subscribe **before** requesting the snapshot.
- Apply a snapshot or event only if its sequence is strictly greater than the held one. Sequences are decimal strings compared as `BigInt`, never `Number`. A feed without sequences uses a local generation counter: an event that arrives while the snapshot is in flight wins over that snapshot.
- A payload flagged for resnapshot triggers a new snapshot request.
- Subscription and snapshot failures are recorded in the query's error state. A later successful write clears the error.
- Start is reference counted per query client: the first consumer starts it, the last stops it and unlistens. A start, stop, start sequence under StrictMode leaves exactly one live subscription and no leaked listener.
- `useFeed(feed)` starts the feed for the calling component's lifetime.

### Queries

- `hostStatusFeed` / `hostStatusOptions`: `commands.hostStatus()` and `events.statusChanged`.
- `appInfoOptions`: `commands.appInfo()` with `staleTime: 'static'`.
- `barFeed` / `barSnapshotOptions`: `commands.barSnapshot()` and `events.barChanged`, ordered by `sequence`, honoring `resnapshot`. The cached value is the whole typed outcome (`available`, `unavailable`, `unauthorized`); `unauthorized` is kept only while nothing with a sequence is held.
- Selectors for the Bar are plain exported functions over the outcome (for example `selectBarView`), so a component subscribes to one slice.

### zustand (`src/ui/src/stores/`)

- `createStoreContext(createState)` returns a `Provider` (creates one vanilla store per mounted provider), a `useStore(selector)` hook that throws a clear error outside the provider, and a `useStoreApi()` escape hatch for imperative access.
- Conventions, documented in `docs/architecture/ui.md`: zustand holds client-only UI state and never host or remote data; state and actions are separate keys (`actions` object) so action selectors are stable; components select one value per hook or use `useShallow`; stores are scoped to a window through the provider unless they are truly app-wide.
- No production store ships in this part (overview decision 15).

## Acceptance criteria

- [x] UI-03-01: The feed applies events and snapshots in sequence order for every interleaving in the test matrix, and never regresses to an older sequence.
- [x] UI-03-02: A sequence above `Number.MAX_SAFE_INTEGER` orders correctly.
- [x] UI-03-03: Mount, unmount, mount under StrictMode leaves one listener; unmounting the last consumer leaves zero; a `listen` that resolves after disposal is unlistened at once and requests no snapshot.
- [x] UI-03-04: Subscription and snapshot failures surface as query errors and clear on the next successful write.
- [x] UI-03-05: Two components selecting different slices of the Bar snapshot re-render only when their own slice changes.
- [x] UI-03-06: Two `Provider`s from one `createStoreContext` hold independent state; the hook throws outside a provider; an object selector with `useShallow` does not loop.
- [x] UI-03-07: `docs/architecture/ui.md` documents the IPC boundary, the query and feed rules and the zustand conventions.
- [x] UI-03-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- `feed.test.ts` with the mocked Tauri boundary: event before snapshot, snapshot before event, stale snapshot after a newer event, equal sequence, resnapshot, listen failure, snapshot failure, recovery, late `listen` resolution, reference counting, StrictMode.
- `host.test.ts` and `bar.test.ts`: option factories, outcome handling for all three Bar outcomes, `unauthorized` retention, `appInfo` never refetching.
- A render-count test for slice subscriptions.
- `createStoreContext.test.tsx` with a sample store defined in the test.
- `tests/` gains typed fixtures for `HostStatus` and `BarSnapshotOutcome` and an IPC mock helper built on `@tauri-apps/api/mocks`. The end-to-end fake host from part 01 gains typed helpers for the same payloads.

## Notes

- Read the host contracts from the generated bindings and the Rust command docs, not from the deleted UI.
- Mutations for Bar click commands arrive with the widgets that use them.
- Decision (orchestrator, during implementation): feed-backed queries use `queryFn: skipToken`, so only the feed writes them and no fetch can overwrite an ordered value. They are read with `useQuery` and a `select`, not `useSuspenseQuery`.
