---
branch: feat/foundation
---

# Modular foundation and proxyAudio pilot

Read when implementing or reviewing the rewrite foundation; the status table records completed and remaining parts.

## Goal

Replace the application core with an owned SDK and one complete module. Adding a tool means adding
a crate, implementing `Module`, declaring its surfaces and registering its factory in the host.
No edits to SDK dispatch switches, global stores or a second lifecycle system.

## Decisions

- macOS 26 minimum; Tauri 2, React, TypeScript, Vite, Linaria, React Query and zustand stay.
- New sources live under `src/crates/` and `src/ui/`; Cargo.toml, configs, scripts and docs stay at root.
  Root Cargo workspace lists `src/crates/{app,cli,sdk,macos}` plus individual
  `src/crates/modules/<crate-id>` members as added, with `_legacy` excluded. Archived paths retain
  their old app/native and app/ui layout; the modules container itself is not a crate member.
- Bun replaces pnpm for the new tree; Vitest browser mode with Playwright WebKit remains the UI runner.
  Retain Rust nightly, rustfmt configuration, workspace Clippy lints/release profile, rust-analyzer,
  cargo-sort and cargo-nextest. Remove markdownlint; oxfmt formats Markdown; oxlint does not replace
  Markdown prose linting. Part 1 verifies pinned tools instead of claiming equivalent coverage.
- Archive old application/build inputs/scripts/docs in self-contained `_legacy/`, retaining its pnpm
  manifest/lockfile and Cargo workspace. New tools exclude it. Only docs/index.md, docs/workflow.md and
  docs/specs/ stay active during the docs move; optional `_docs/` moves too. No move during spec-writing.
- New product `Stache Next`, bundle ID `com.marcosmoura.stache.next`, executable `stache`.
  Runtime/cache/instance locks are isolated; configuration directory sharing is intentional.
- Config: `$XDG_CONFIG_HOME/stache/` or `~/.config/stache/`, `global.jsonc` plus exact module-ID filenames
  (`proxyAudio.jsonc`). Loader never discovers legacy config. Choose `jsonc-parser` with explicit
  comments/trailing-comma support, disabling other JSON extensions. Rust defaults/schema metadata
  generate commented templates and committed per-file schemars schemas.
- Keep `execOnStartup` global (string-or-list), executed once per desktop launch, never on reload/CLI.
- One binary selects desktop/CLI. Contributions must work without starting Tauri, lifecycle, tray,
  socket server or device-changing listeners. Modules never depend on one another directly.
- Unix socket control only; no distributed notifications. No process-wide mutable singletons.
  All project-authored unsafe native integration belongs in `stache-macos` behind safe APIs.
- Drop tauri-plugin-zustand in the new host: config is file-backed, zustand is UI-local state; no
  existing cross-webview persisted state needs porting. Keep settings architecture file-first.

## Architecture

Arrows mean “depends on”; SDK cannot import Tauri or native adapters.

```text
src/crates/app (stache binary; Tauri host/composition root)
  ├── src/crates/cli ──────────────────────────> src/crates/sdk
  ├── src/crates/modules/proxy-audio ──> src/crates/sdk + src/crates/macos
  ├── src/crates/macos ────────────────────────> src/crates/sdk
  └── src/crates/sdk (contracts, registry, config, events, IPC)
src/ui/
  index.html, index.tsx, main.tsx, main.css
  tests/setup.ts                      support/fixtures/mocks only, no test suites
  src/
    sdk/                              generated bindings/client, tokens, SDK exports
    shell/
      components/ComponentName/       ComponentName.tsx, .test.tsx, .styles.tsx, index.ts
      modules/moduleName/             moduleName.tsx, .test.tsx, .styles.tsx, index.ts
    utils/                            utilName.ts, index.ts; only when needed
_legacy/                              independent old application, build system and docs
```

Components and modules are siblings under shell, not nested in one another. Tests are colocated;
the diagram's test/style suffixes share their containing component/module basename. `main.tsx`
bootstraps the root exported/rendered by `index.tsx`; Vite root is `src/ui` and requires index.html.
Tray stays native; part 1 adds no speculative React tray/bar modules. Pilot has no UI surface.

CLI accepts descriptors/executors from app composition; it imports neither app nor module crates.
Cross-module needs use explicit context capabilities/events, added only for real consumers. No
dynamic plugin ABI or speculative service ecosystem.

## Module contract summary

Typed config and `configure → start → reconfigure → stop`, with one type-erasure boundary at registration.
Descriptor declares ID, config/schema/defaults, permissions, thread requirements, external dependencies,
CLI/IPC handlers and optional UI surfaces. Context provides owned logging/events, cancellation, named
workers and main-thread execution. Status distinguishes disabled/running/paused/permission-waiting/
unavailable/failed. Config diagnostics are separate: bad reload does not falsely stop a running module.

## Non-goals

Porting tiling, bar, wallpapers, keybindings/hotkeys/Caps Lock, MenuAnywhere, noTunes, Hold-to-Quit,
Keep Awake or their commands. No Settings UI, cutover, deletion of `_legacy/` or dynamic module loading.
Widgets are permanently absent from the new UI/native/config tree; archived legacy retains its old
behavior until parity/removal.

## Parts and acceptance ownership

Acceptance checkboxes live in exactly one part; IDs below map goals to their sole owners.

| Part                    | Summary                                         | Criteria | Status |
| ----------------------- | ----------------------------------------------- | -------- | ------ |
| [01](01-repo-reset.md)  | Buildable archive, Bun tooling, empty tray app  | F01–F07  | done   |
| [02](02-sdk-core.md)    | Owned lifecycle/threading/errors/events SDK     | S01–S07  | done   |
| [03](03-config.md)      | Typed files, generation, reload and migration   | C01–C09  | done   |
| [04](04-ipc-cli.md)     | Safe socket transport and standalone CLI        | I01–I07  | done   |
| [05](05-host-ui.md)     | Host/tray/permissions/shutdown and typed UI SDK | H01–H10  | done   |
| [06](06-proxy-audio.md) | Audio parity and comparative measurements       | A01–A07  | done   |

Parts are ordered, individually buildable/testable/committable; all must be done before merge.
Boundary adjustment: part 3 introduces the pilot's config-only crate so migration/schema output has
one authoritative type; part 6 adds runtime. Part 3 exposes local config commands through the minimal
CLI scaffold; part 4 completes composition/transport. Part 1 tray is integrated with registry in part 5.

## Affected docs

Planning-stage [reference drafts](reference/overview.md) stay inside the retained specs tree. Existing
behavior docs stay accurate until archived in part 1. Each implementation part materializes/updates
its named lasting docs from these drafts. No planned API is presented as shipped. Part 1 routes legacy
features to `_legacy/docs/`; new routes appear when implemented. Lasting docs start with “Read when…”
after title and stay near 150 lines.

Targets: docs/architecture/{overview,sdk,config,ipc,ui}.md, docs/guides/{new-module,testing,performance}.md,
docs/features/proxy-audio.md; root README/AGENTS, docs/workflow and .agents/roles/*.md change in part 1.

## Evidence and open questions

- Current [tauri-specta docs](https://github.com/specta-rs/tauri-specta/blob/main/_autodocs/configuration.md)
  recommend Tauri 2, specta/tauri-specta `=2.0.0-rc.25`, specta-typescript `0.0.12`.
  Commands, events and explicit `.typ::<T>()` export are documented. Part 5 compiles a compatibility
  spike against the locked Tauri version first. Generic JSON dispatch does not generate typed methods.
- [Oxfmt support](https://oxc.rs/docs/guide/usage/formatter/language-support) documents Markdown;
  installed oxfmt 0.64.0 processed these Markdown files during spec validation. The
  [oxlint support list](https://oxc.rs/docs/guide/usage/linter) covers JS/TS and framework script blocks,
  not Markdown prose. No prose lint replacement is promised; recheck pinned tools in part 1.
- [jsonc-parser](https://docs.rs/jsonc-parser/latest/jsonc_parser/) documents serde conversion and
  explicit trailing-comma options. Pin/test compatible version in part 3.
- No product-scope questions remain. Implementation gates: verify binding-toolchain compatibility and
  pinned formatter support; report blockers rather than inventing support.
- Performance budgets remain provisional until part 6 records named-machine/build measurements;
  report misses for an explicit decision, never invent numbers or silently relax targets.
