# Adding a module

Read when creating a module crate or adding lifecycle behavior to one.

A module is a crate under `src/crates/modules/<crate-id>` that depends on
`stache-sdk` (and `stache-macos` only when it needs native access). No module
depends on another module; use declared services, context capabilities or typed
events instead. Add the crate path to the root `Cargo.toml` workspace `members`
list when it is created.

## Skeleton

A module's configuration type is usually owned by its own crate, so the same
type drives runtime, safe schema generation and migration. Register it in the
host catalog (`src/crates/app/src/catalog.rs`) and derive the standard traits:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct TickerConfig {
    /// Whether the module should run.
    pub enabled: bool,
    /// Tick interval in milliseconds.
    pub interval_ms: u64,
}

impl TickerConfig {
    /// The catalog identifier and file stem (`ticker.jsonc`).
    pub const ID: ModuleId = ModuleId::new("ticker");
}

impl ModuleConfig for TickerConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.interval_ms == 0 {
            return Err(ConfigError::new("interval_ms must be greater than zero"));
        }
        Ok(())
    }
}
```

Doc comments become schema descriptions and template comments. Field names in
serde are the user-facing keys; defaults must be real `Default` values, not a
hand-written template. Add the crate to the root workspace members and register
it in the app catalog; registration is the only type-erasure boundary.

Module lifecycle code stays out of the config crate: the registry starts,
reconfigures and stops; `ConfigStore` only reads files and calls `reload`.

```rust
use stache_sdk::{ConfigCatalog, ModuleId};

let mut catalog = ConfigCatalog::new();
catalog.register::<TickerConfig>(TickerConfig::ID)?;
```

The config file is `~/.config/stache/ticker.jsonc` (or the selected
`--config` directory). Comments and trailing commas are accepted; every other
JSON extension is rejected. Missing files use the typed default; malformed
files leave the module inactive with a diagnostic instead of silently using
defaults. Keys the type does not declare are ignored with a warning and do not
reject the file.

## Module skeleton

The lifecycle implementation uses the configuration type above and only adds
`Module`:

```rust
use stache_sdk::{Module, ModuleContext, ModuleDescriptor, ModuleId, ModuleResult};

pub struct Ticker;

impl Module for Ticker {
    type Config = TickerConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: ModuleId::new("ticker"),
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
        }
    }

    fn configure(&mut self, config: &TickerConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        ctx.logger().info(format!("interval {} ms", config.interval_ms));
        Ok(())
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        ctx.spawn_worker("tick", move |token| {
            while !token.is_cancelled() {
                let _ = token.wait(std::time::Duration::from_millis(250));
            }
        })?;
        Ok(())
    }

    fn reconfigure(&mut self, _config: &TickerConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        // Prepare everything that can fail before replacing live state.
        ctx.logger().info("reconfigured");
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        // Stop is idempotent and must be safe after a partial start.
        ctx.logger().info("stopped");
        Ok(())
    }
}
```

`start` must not leave resources behind when it returns an error; the registry
also calls `stop` immediately after a failed configure/start, so `stop` must be
safe after a partial start. `reconfigure` must leave the previous runtime intact
when it returns an error. `stop` is called after the runtime token is cancelled
and before its workers are joined.

## Registering

The host composition root owns the registry. There is no global registration
function.

```rust
// Inside a function returning `stache_sdk::RegistryResult<()>`.
let registry = Registry::new(
    logger,
    events,
    main_thread,
    permissions,
    Arc::new(stache_sdk::UnavailableWindows),
);
let handle = registry.register(Ticker, TickerConfig { enabled: true, interval_ms: 500 })?;
registry.start_all()?;
```

Production factories live in `src/crates/app` and call
`host::register_module` so the initial file, `register_rejected` and
`ConfigStore::bind` stay one path. The host, not the module, owns the registry,
socket and tray. A module crate exports its `Module` and config type; it does not
start Tauri and is not imported by the CLI.

Registration validates the config immediately and returns a typed
`ModuleHandle<TickerConfig>`; keep it and pass it to `reload` with the same
configuration type. An invalid initial config leaves the module `Stopped` with a
diagnostic and revision `0`, not `Disabled`. Duplicate module IDs and duplicate
service providers, self-dependencies and cycles are rejected with typed
`RegistryError` values before anything starts.

## Windows

Declare each surface the module may open in `ModuleDescriptor::surfaces`, then
request it through the context:

```rust
use stache_sdk::{SurfaceId, WindowKey, WindowKind, WindowRequest, WindowSize};

fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
    let request = WindowRequest::new(
        WindowKey::new("main").expect("static key"),
        SurfaceId::new("ticker"),
        WindowSize::new(800, 600).expect("static size"),
        WindowKind::Standard,
    );
    let window = ctx.windows().request(&request).map_err(|error| {
        stache_sdk::ModuleError::new(
            ctx.id(),
            stache_sdk::Operation::Start,
            stache_sdk::ErrorCategory::Platform,
            error.to_string(),
        )
    })?;
    self.window = Some(window);
    Ok(())
}
```

Only `Standard` is implemented. `Launcher` and `NonActivatingPanel` return
`WindowError::UnsupportedKind`; arbitrary URLs, native flags, unsafe surface
spellings and undeclared surfaces are rejected. The same owner/key reuses one
window and shares a lease: dropping each `OwnedWindow` releases only that lease,
and the window closes with the last one. A retained capability from a stopped
runtime rejects new requests; the registry still releases all of the module's
windows when it stops, including after a failed start.

## Lifecycle rules

- A descriptor is readable without starting the module; declare only real
  services, permissions, executables and windows surfaces. Executables must
  resolve to an executable file on `PATH`; GUI apps have a minimal `PATH`, so keep
  the list small and resolvable.
- Disabled modules hold no resources. Pause is temporary and never edits config.
- Reload of a running module calls `reconfigure`; a paused or disabled module
  only updates its next-start snapshot.
- An unchanged config is skipped. A rejected candidate keeps the last accepted
  revision and records a diagnostic separately from the status.
- Start order follows `requires`/`provides`; shutdown reverses successful
  starts. A failed start does not block other modules.
- If the module provides a service, disable/pause/failed start/permission loss
  stop its running consumers with `Unavailable(MissingService)`; recovery
  restarts enabled, non-paused consumers. Recovery never starts disabled or
  paused modules.
- `stop` runs immediately after a failed configure/start; a cleanup failure
  keeps `needs_stop` set but does not replace the original error.

## Threads and events

- The registry owns workers. Use `ctx.spawn_worker` with a descriptive name (or
  `ctx.workers().spawn` for the raw handle); never detach a thread or keep a
  `JoinHandle`.
- Observe `ctx.cancellation()`; the registry cancels it before `stop`.
- `ctx.events().publish(event)` never blocks and never calls subscribers
  inline. A slow UI subscribes with a bounded queue and counts drops.
- Main-thread work goes through `ctx.main_thread().execute(...)`; it runs inline
  when already on the main thread and otherwise enqueues.
- Log with `ctx.logger()` so every record carries the module and operation.
- Do not hold a lock while calling into native code or a worker loop.

The context exposes no registry and no other module's implementation. If a
module needs another capability, add a real service declaration and recovery
path rather than a direct dependency.

## CLI and IPC surfaces

A module may contribute a command and a socket handler without importing
another module and without starting the host.

```rust
use clap::Command;
use stache_sdk::{CliContribution, CliExecutor};

struct List;

impl CliExecutor for List {
    fn execute(
        &self,
        _matches: &clap::ArgMatches,
        _ansi: bool,
        out: &mut dyn std::io::Write,
        _err: &mut dyn std::io::Write,
    ) -> std::process::ExitCode {
        // Local enumeration or an `IpcClient` request; never start the host.
        let _ = writeln!(out, "[]");
        std::process::ExitCode::SUCCESS
    }
}

let contribution = CliContribution::new(|| Command::new("audio"), List);
```

Register contributions in `src/crates/app/src/commands.rs`; the CLI crate only
sees the generic `CommandCatalog`. Commands must not duplicate a built-in.
`completions` is generated from that same catalog, so a registered command is
listed and an unregistered one is not.

For socket methods, implement `IpcHandler` and register it with the host's
`Router`. `handle` receives the method name and raw `params`; deserialize once
with `decode_params`, and serialize the typed result with `encode_response`.
Unknown methods, disabled modules, unavailable dependencies and invalid
parameters have distinct `HandlerError` constructors, so clients can branch on
the kind instead of the message. `core` is reserved for reload and status.
See [ipc.md](../architecture/ipc.md) for the wire shape and limits.

A factory contributes handlers through `ipc_handlers`, which receives a clone of
the app's read-only `FactoryContext`:

```rust
fn ipc_handlers(
    &self,
    context: &FactoryContext,
) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
    vec![("ticker", Arc::new(TickerIpc::new(context.clone())))]
}
```

The context wraps a private registry and store and exposes only
`module_snapshot(id)`, `diagnostic(id)` and `accepted_watch(handle)`: published
lifecycle metadata, the held file diagnostic and the typed accepted
configuration cell. It offers no mutable registry or store, no configuration
apply or write and no disk access, so a handler cannot start, stop or reconfigure
a module while composing a response. Use it for a read-only app-owned status
envelope such as Keep Awake's; all policy and runtime ownership stay in the
module. See [keep-awake.md](../features/keep-awake.md) for that contract.
Existing factories keep their registration and runtime behavior; only their
handler signature gains the context.

`accepted_watch` returns `AcceptedWatch<C>` for the exact configuration type the
module registered. `read()` is coherent and disk-free; `wait_for_change` supplies
a bounded, cancellable revision wait:

```rust
let watch = context.accepted_watch(handle)?;
if let Some(accepted) = watch.read() {
    let revision = accepted.revision;
    let config = accepted.config.as_ref();
}
```

A later part uses that pair instead of a module-local cache or current disk when
answering a request. A handle from another registry with the same identifier is
rejected with `RegistryError::ConfigTypeMismatch`. See
[config.md](../architecture/config.md#accepted-configuration).

A configuration-only module such as Wallpapers part 1 can be registered in the
catalog without a factory or `Module`: the watcher and `reload_all` skip its
unbound file, `core.status` marks it valid but not active and no warning storm
or startup failure results. Add the runtime only when a later part needs it.

## Pilot example: proxyAudio

The shipping pilot combines the pieces above. It is the reference for a module
with a native watcher, a CLI command and a socket method:

- `src/crates/modules/proxy-audio/src/watcher.rs` implements `Module` over an
  injected `stache_macos::audio::AudioBackend`. `configure` stores the accepted
  snapshot. `start` registers one listener generation, applies the policy once
  and spawns one named worker; a worker-spawn failure drops the subscription.
  `reconfigure` swaps the shared snapshot and re-evaluates once without
  resubscribing. `stop` drops the subscription; the registry cancels and joins
  the worker. The module keeps no global state and forbids unsafe code.
- `src/crates/modules/proxy-audio/src/cli.rs` returns a `CliContribution`
  whose executor enumerates locally. It never starts the host or a listener and
  receives the shared `ansi` flag for its JSON output.
- `src/crates/modules/proxy-audio/src/ipc.rs` implements `IpcHandler` for the
  read-only `list` method.
- `src/crates/app/src/host.rs` has one `ModuleFactory` that registers the module
  and returns its `ipc_handlers(context)`, both over one shared backend;
  `ModuleFactories::production(main_thread, power_owner)` is the base list used
  by `run_with`, which then adds `WallpapersFactory` with `.with(...)` because
  that factory needs the identity paths, configuration directory and home;
  `power_owner` is the composition root's shared Keep Awake assertion owner.
  `src/crates/app/src/bindings.rs` exports the listing DTOs, not a Tauri
  command, because the pilot has no UI.

Read [features/proxy-audio.md](../features/proxy-audio.md) for the selection
rules and invariants. The same split applies to a new module: policy and tests in
the module crate, native calls in `stache-macos`, and one factory plus one
contribution in the app.

## Tests

Fake modules in `stache-sdk` are the reference pattern. A module crate should
test its own failpoints and state, not the registry:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_interval_is_rejected() {
        let config = TickerConfig { enabled: true, interval_ms: 0 };
        assert!(config.validate().is_err());
        assert!(config.enabled());
    }
}
```

Register a fake host in tests with an injected logger, event bus, main-thread
executor, permission provider and window backend. Never prompt for a real
permission in a test.
For executable checks, use a name that is guaranteed absent instead of a tool
that may or may not be installed on the machine.

```sh
cargo nextest run -p <crate-id>   # this module only
cargo nextest run -p stache-sdk   # SDK lifecycle and fake-module tests
bun run test                      # full UI + native suite
bun run lint
bun run format:check
```

The real Accessibility prompt and the app-harness main-thread smoke test run
only in the macOS host, never in unit tests.
