# Target architecture

Read when reviewing the planned foundation; this is a design draft, not shipped behavior.

Materialize as `docs/architecture/overview.md` incrementally in parts 1, 2 and 5. Existing architecture
docs describe legacy until archived; these drafts live inside specs so the archive cannot swallow them.

## Boundaries

- `src/crates/sdk`: owned lifecycle, config, threading, events/errors and transport contracts.
- `src/crates/macos`: safe native adapters; only home for project-authored unsafe FFI.
- `src/crates/modules/<crate-id>`: independent tools, importing SDK/macOS plus external libraries only.
- `src/crates/cli`: clap composition and output, importing SDK; no host/module imports.
- `src/crates/app`: stache binary, Tauri host/composition root, registration, native tray and shutdown.
- `src/ui/src/sdk`: generated client/foundations; `src/ui/src/shell/{components,modules}`: sibling
  component and optional module-surface directories, each with named TSX/test/styles.tsx and index.ts.
- `src/ui/src/utils`: utilName.ts/index.ts only as needed, with colocated tests. Frontend root
  `src/ui/{index.html,index.tsx,main.tsx,main.css}` retains Vite HTML and bootstrap/root split.
  `src/ui/tests/setup.ts` and support fixtures/mocks are not test-suite locations.

Root Cargo.toml lists `src/crates/{app,cli,sdk,macos}` plus individual module crate paths as added,
not the modules container, and excludes `_legacy`; scripts/config/docs stay root.
Vite root is src/ui, tests colocate under src/ui/src; no new source paths
are prefixed onto archived `_legacy/app/native` or `_legacy/app/ui` historical references.

No module starts during catalog construction or standalone CLI execution. App assembles factories and
passes commands/services to CLI. New module registration is a composition-root edit, not SDK branching.

## Ownership

Host owns registry, configs, runtime socket, permission monitor, windows and cancellation roots.
ModuleContext scopes capabilities; no process-wide mutable manager or lifecycle singleton.
SDK does not depend on Tauri/macOS; macOS adapters implement SDK-facing traits where needed.

Desktop starts tray before potentially slow module work. Quit/signals/exit converge on one idempotent
cleanup path; reload reconciles files without restart. No module-requested window means zero webviews.

## Identity and archive

New product Stache Next uses com.marcosmoura.stache.next and macOS 26 minimum. Cache and runtime state
do not overlap legacy. Shared ~/.config/stache (or XDG override) is intentional: new files differ.
Legacy app remains independently buildable under `_legacy`, excluded from new quality tooling.
Widgets have no place in the new app; other unported features remain legacy-only until later specs.

## Read next

- [SDK](sdk.md): module state, resource ownership and threading.
- [Config](config.md): last-good reload, templates and migration.
- [IPC](ipc.md): wire routing, standalone commands and identity paths.
- [UI](ui.md): generated binding boundaries and on-demand windows.
- [New module](new-module.md): minimal extension recipe.
- [proxyAudio](proxy-audio.md): pilot policy/lifecycle.
- [Validation](validation.md): tests, tooling and measurement protocol.
