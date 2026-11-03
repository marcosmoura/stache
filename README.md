# Stache Next

Read when building the macOS 26+ tray-only foundation.

The new app has a tray icon and Quit action, with zero webviews. No desktop tools, configuration,
IPC or CLI commands ship yet. The React shell is available only through Vite/browser tests.
The SDK, native adapter and CLI crates in `src/crates/` reserve boundaries, not future APIs.
The frontend lives at `src/ui/`, with colocated components/tests/styles under `src/ui/src/shell/`.

## Build

Install Bun **1.4.4**, Node **26** (local tool CLIs), Xcode command-line tools and the Rust toolchain
in `rust-toolchain.toml`. Install `cargo-sort` and `cargo-nextest` for quality gates.

```sh
bun install --frozen-lockfile
node_modules/.bin/playwright install webkit
bun run lint
bun run format:check
bun run test
bun run test:ui:coverage
bun run build
bun run tauri:build
```

The bundle is `target/release/bundle/macos/Stache Next.app`. It uses
`com.marcosmoura.stache.next`; it does not install over or terminate Stache. It requires no
media-control resources or device permissions. Bundles use ad-hoc signing by default; distribution
signing/notarization is a separate prerequisite, not configured here. Local ad-hoc signing is explicit
in the bundle configuration.

`bun run tauri:dev` runs the tray host and Vite; `bun run dev` serves the browser shell alone.
`bun run format` is the explicit formatting operation. Lint, checks and hooks never fix source.

## Documentation

- [Docs index](docs/index.md), [architecture](docs/architecture/overview.md),
  [validation](docs/guides/testing.md), [agent workflow](docs/workflow.md).
- [Legacy build instructions](_legacy/README.md). The old application, identity and pnpm workspace
  are retained independently in `_legacy/`; root tools exclude it.
- [Foundation plan](docs/specs/2026-11-02-foundation/00-overview.md). Later parts are not shipped.

Never run legacy and new device-changing listeners together when future modules are introduced.
