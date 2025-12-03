```instructions
# Barba Shell - AI Coding Instructions

## Project Overview

Barba Shell is a **macOS-only** Tauri 2.x desktop application providing a status bar with tiling window manager integration. It uses a monorepo architecture with three main packages:

- **Desktop App** (`packages/desktop/`): React 19 + TypeScript frontend with Tauri 2.x Rust backend
- **CLI** (`packages/cli/`): Standalone Rust CLI built with Clap for controlling the desktop app
- **Shared** (`packages/shared/`): Shared Rust types (config, schema generation) used by both CLI and desktop

## Repository Structure

```

barba/
├── 📁 scripts/ # Build and deployment scripts
├── 📁 packages/
│ ├── 📁 cli/ # Standalone CLI application (Rust + Clap)
│ │ └── 📁 src/
│ │ ├── main.rs # CLI entry point
│ │ ├── commands.rs # Clap command definitions
│ │ ├── ipc.rs # IPC client for desktop communication
│ │ └── error.rs # Error types
│ ├── 📁 desktop/
│ │ ├── 📁 tauri/ # Tauri Rust backend
│ │ │ └── 📁 src/
│ │ │ ├── lib.rs # Tauri app entry, command registration
│ │ │ ├── ipc.rs # IPC server for CLI communication
│ │ │ ├── bar/ # Bar components
│ │ │ ├── config/ # Configuration (wraps shared types)
│ │ │ └── wallpaper/ # Wallpaper management
│ │ └── 📁 ui/ # React frontend
│ │ ├── main.tsx # React app entry
│ │ ├── bar/ # Bar UI components
│ │ ├── hooks/ # React hooks (useTauriEventQuery, etc.)
│ │ └── design-system/ # Styling tokens and utilities
│ └── 📁 shared/ # Shared Rust crate
│ └── 📁 src/
│ ├── lib.rs # Crate entry, re-exports
│ └── config.rs # Config types, schema generation
├── Cargo.toml # Workspace root
├── package.json # pnpm workspace root
└── vite.config.ts # Vite configuration

```

## Architecture

### CLI ↔ Desktop Communication

The CLI communicates with the running desktop app via Unix socket IPC:

```

CLI (barba reload) → Unix Socket → Desktop IPC Server → Tauri Event → Frontend

```

- CLI sends commands to `~/.local/run/barba.sock` (or `$XDG_RUNTIME_DIR/barba.sock`)
- Desktop's `ipc.rs` module listens and dispatches events to the frontend

### Data Flow Pattern (Desktop App)

```

Rust Backend (packages/desktop/tauri/) → Tauri Events/Commands → React Query (ui/) → UI Components

````

1. **Rust services** in `packages/desktop/tauri/src/bar/components/` expose `#[tauri::command]` functions
2. **Frontend services** in `packages/desktop/ui/bar/*/` use `invoke()` from `@tauri-apps/api/core`
3. **React components** use `useTauriEventQuery` hook to subscribe to real-time events

### Key Integration Pattern: `useTauriEventQuery`

Located in `packages/desktop/ui/hooks/useTauriEventQuery.ts`:

```typescript
const { data } = useTauriEventQuery<PayloadType>({
  eventName: 'tauri_event_name',
  initialFetch: () => invoke('rust_command_name'),
  transformFn: (payload) => transformedData,
});
````

### Component Structure Convention

Each bar feature follows this structure:

```
ComponentName/
├── index.ts                  # Re-exports
├── ComponentName.tsx         # React component
├── ComponentName.styles.ts   # Linaria CSS (css`` tagged templates)
├── ComponentName.service.ts  # Tauri invoke calls & business logic
└── ComponentName.types.ts    # TypeScript interfaces
```

See `packages/desktop/ui/bar/Status/Battery/` as a reference implementation.

## Styling Conventions

- Use **Linaria** (`@linaria/core`) for CSS - exports named CSS class constants:
  ```typescript
  export const button = css`...`;
  export const buttonActive = css`...`;
  ```
- Style files named `*.styles.ts` - automatically processed by `@wyw-in-js/vite`
- Use design tokens from `packages/desktop/ui/design-system/` (Catppuccin Mocha colors)
- Combine classes with `cx()` from `@linaria/core`

## Rust Backend Patterns

- Commands in `packages/desktop/tauri/src/bar/components/*.rs` - register in `lib.rs` via `tauri::generate_handler![]`
- Use `#[tauri::command]` attribute for frontend-callable functions
- Events emitted via `window.emit("event_name", payload)` or `app_handle.emit()`
- Strict Clippy lints enabled: `pedantic`, `nursery`, `cargo` warnings

## CLI Commands

The standalone CLI (`barba`) provides:

```bash
barba reload                        # Reload configuration
barba focus-changed                 # Notify focus change (for window manager integration)
barba workspace-changed <name>      # Notify workspace change
barba wallpaper set <action>        # Set wallpaper (next/previous/random)
barba wallpaper set --f <filename>  # Set wallpaper by filename
barba wallpaper generate-all        # Pre-generate all wallpapers
barba generate-schema               # Output JSON schema for config
```

## Development Commands

```bash
pnpm dev                # Start Vite dev server (frontend only)
pnpm tauri:dev          # Full app with hot reload
pnpm tauri:build        # Build production app
pnpm build:cli          # Build CLI binary
pnpm test:ui            # Vitest browser tests
pnpm test:tauri         # Rust tests via cargo-nextest
pnpm lint               # ESLint/Stylelint + Clippy
pnpm format             # Prettier + cargo fmt
```

## Testing Conventions

- Frontend tests use Vitest with `vitest-browser-react` for component testing
- Test files co-located: `ComponentName.test.tsx` alongside source
- Rust tests inline with `#[cfg(test)]` modules in the same file

## Path Aliases

- `@/` maps to `./packages/desktop/ui/` (configured in `vite.config.ts` and `tsconfig.app.json`)

## Critical Files

- `packages/desktop/tauri/src/lib.rs` - Tauri app entry, command registration, plugin setup
- `packages/desktop/tauri/src/ipc.rs` - IPC server for CLI communication
- `packages/desktop/ui/hooks/useTauriEventQuery.ts` - Core pattern for Tauri-React integration
- `packages/cli/src/main.rs` - CLI entry point with Clap
- `packages/shared/src/config.rs` - Shared config types and schema generation
- `Cargo.toml` - Workspace root defining all Rust packages

## Additional Notes

- The app is macOS-only due to dependencies on macOS-specific features (e.g., wallpaper management, status bar integration).
- Follow existing code patterns closely for consistency.
- After any iteration, run `pnpm lint` and `pnpm format` to ensure code quality.
