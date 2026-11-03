---
status: done
---

# 02 — SDK core

Read when implementing lifecycle ownership and testing it with fake modules.

## Scope and files

Create src/crates/sdk/src/{module,context,registry,status,permissions,threading,error,logging,events,
dependencies}.rs and tests; src/crates/macos/src/{accessibility,main_thread}.rs wraps native APIs.
No real audio listeners or host permission polling yet.

## Key APIs (sketches)

```rust
trait Module: Send + 'static {
    type Config: ModuleConfig;
    fn descriptor(&self) -> ModuleDescriptor;
    fn configure(&mut self, config: &Self::Config, ctx: &ModuleContext) -> Result<()>;
    fn start(&mut self, ctx: &ModuleContext) -> Result<()>;
    fn reconfigure(&mut self, config: &Self::Config, ctx: &ModuleContext) -> Result<()>;
    fn stop(&mut self, ctx: &ModuleContext) -> Result<()>;
}
fn register<M: Module>(&mut self, module: M) -> Result<ModuleHandle>;
```

ModuleConfig supplies serde/schema/defaults, validation and enabled state. Registry adapters erase
config types once. ModuleContext provides scoped log/event access, cancellation, named worker spawning,
main-thread executor and explicit capabilities; no mutable global registry/Tauri AppHandle. Descriptors
are usable without start. Add concrete CLI/IPC/window fields in parts 4/5, not speculative abstractions.

## Requirements

- One host-owned registry serializes per-module operations, releases locks before module/native calls
  and rejects duplicate IDs. Service prerequisites order startup; reverse successful startup orders
  shutdown. No direct runtime module references; inject real service needs only.
- Status: Stopped, Disabled, Running, Paused, WaitingForPermission, Unavailable(DependencyFailure),
  Failed(ModuleError). Store desired enabled flag, temporary pause, last-good revision and config
  diagnostic separately. Invalid initial config is Stopped with diagnostic, not disabled-by-config.
- Disable stops resources; pause stops without file edits. Paused reload updates snapshot, not runtime.
  Disabled→enabled clears pause. Permission/dependency recovery starts only enabled/non-paused modules.
  Unchanged config is no-op. Configure stages without starting workers; reconfigure is transactional,
  preparing before swapping live resources. Rejected config keeps prior state/revision; actual runtime
  resource loss is Failed, never confused with a rejected candidate.
- Stop is idempotent and handles partial starts. Failed start cleans resources and does not block other
  modules. Retry on explicit resume/reload or prerequisite recovery, not busy looping. Declare external
  executable/framework requirements and report actionable Unavailable; no foundation sidecars.
- PermissionProvider injects fresh check and explicit request. Registry handles grant/revocation;
  native polling/prompt policy is part 5, never cached forever.
- Main-thread jobs run inline if already there; off-main jobs dispatch safely without cyclic waits.
  Named workers carry cancellation/join owner; cancel before join, join off-main. Never detach workers
  or free callback data while native callers can still access it.
- Typed errors carry module ID, operation, category (config/permission/dependency/platform/IPC/lifecycle)
  and source. No Result<_, String> lifecycle. Events have typed payloads, RAII subscriptions and bounded
  queues with documented overflow behavior; consumers cannot block native callbacks. Scoped host-owned
  logging supports configurable levels and multiple test hosts without competing global initialization.
- macos crate documents unsafe invariants behind safe public APIs. SDK/modules/CLI/app forbid
  project-authored unsafe code. Tauri safe APIs remain available in host.

## Acceptance criteria

- [x] S01: Fake modules cover registration/lifecycle; one registry owns transitions, no mutable globals.
- [x] S02: Disabled/pause/wait/unavailable/failure transitions match rules; diagnostic rejection does not
      falsely replace running status.
- [x] S03: Toggle/reload/shutdown races serialize without lock-held native calls, deadlocks, duplicate
      starts or leaked partial-start resources.
- [x] S04: Main-thread dispatch, named-worker cancellation/join and event disposal/overflow are tested.
- [x] S05: Permission/dependency loss/recovery affects only dependents; missing/cyclic service declarations
      fail before startup with typed reasons.
- [x] S06: Logs/errors retain module/operation context; multiple test hosts work without global log state.
- [x] S07: Safe native trust/dispatch adapters document ownership/thread invariants; other new crates
      forbid unsafe code.

## Tests and validation

Fake modules with configure/start/reconfigure/stop failpoints; duplicate IDs, dependency cycles,
grant-after-disable, paused reload, callback/worker races, repeated cleanup and slow subscribers.
Run `bun run lint`, `bun run test`, `bun run format:check`,
`cargo nextest run -p stache-sdk -p stache-macos`. Native CI tests never prompt; main-thread smoke
test runs in macOS app harness.

## Docs

Write docs/architecture/sdk.md, update overview/testing; add lifecycle/thread sections of
docs/guides/new-module.md, runnable with a fake module before audio exists.
