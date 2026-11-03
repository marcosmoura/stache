# SDK contract

Read when designing a module or reviewing lifecycle ownership; planned part 2 documentation.

Materialize as `docs/architecture/sdk.md` after the implementation establishes exact symbols.

## Module and context

`src/crates/sdk/src/module.rs::Module` associates one typed config with configure/start/reconfigure/stop.
Configure prepares without launching work. Start acquires owned resources. Reconfigure prepares before
swapping live state. Stop is idempotent and safe after partial start. Error cleanup is the module's duty.

Descriptor declares stable ID, config metadata, permissions, thread requirements and external dependencies.
CLI/IPC/window fields are added only as needed in parts 4/5. No dynamic loader or broad plugin ABI.
Context supplies scoped logging/events, main-thread dispatch, cancellable named workers and services.
It does not expose a Tauri handle or another module's concrete implementation.

## Registry state

Desired enabled flag and temporary pause are independent of observed operational status. Disabled config
stops resources; pause does not write config. Reconfigure while paused updates the next-start snapshot.
Enabling from disabled clears temporary pause. Permission/dependency recovery never overrides disable/pause.

Status distinguishes Disabled, Running, Paused, WaitingForPermission, Unavailable, Failed and Stopped
(configured runtime not yet started). A separate per-file diagnostic describes invalid candidate config.
Rejecting reload retains last-good revision/runtime; initial invalid config is Stopped with diagnostic,
not Disabled. Genuine start/runtime failure is Failed with structured operation/source information.

Registry serializes operations per module, not native calls under a shared lock. Service prerequisites
are checked before startup; shutdown reverses successful startup. Missing binaries/frameworks produce
Unavailable with an actionable reason, never an untyped string error or hidden crash.

## Resources and threads

Main-thread executor runs inline on-main. Avoid worker→main→worker synchronous waits. Cancellation
precedes off-main joins; stop unregisters/quiesces native callbacks before freeing callback data.
Events use typed payloads and scoped subscriptions with bounded delivery. Native callbacks enqueue
notifications rather than execute policy or block. Scoped host logging is injectable in tests.

Project-authored unsafe belongs only in `src/crates/macos`, documented once behind safe APIs. SDK and
module crates forbid unsafe code. Tests use injected platform/permission services and lifecycle failpoints.
