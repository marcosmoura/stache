```instructions
# Barba Shell - AI Coding Instructions

## Project Overview

Barba Shell is a **macOS-only** Tauri 2.x desktop application providing a status bar with a built-in tiling window manager. It uses a monorepo architecture with three main packages:

- **Desktop App** (`packages/desktop/`): React 19 + TypeScript frontend with Tauri 2.x Rust backend
- **CLI** (`packages/cli/`): Standalone Rust CLI built with Clap for controlling the desktop app
- **Shared** (`packages/shared/`): Shared Rust types (config, schema, tiling types) used by both CLI and desktop

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
│ │ │ ├── ipc/ # IPC server for CLI communication
│ │ │ │ ├── mod.rs
│ │ │ │ ├── server.rs # Unix socket server
│ │ │ │ ├── handlers/ # Command handlers
│ │ │ │ └── types.rs # IPC message types
│ │ │ ├── bar/ # Bar components (battery, cpu, media, etc.)
│ │ │ ├── config/ # Configuration (wraps shared types)
│ │ │ ├── tiling/ # Tiling window manager
│ │ │ │ ├── manager/ # Window manager core
│ │ │ │ ├── layout/ # Layout algorithms
│ │ │ │ ├── window/ # Window operations
│ │ │ │ ├── workspace/ # Workspace management
│ │ │ │ ├── screen/ # Screen/display handling
│ │ │ │ ├── animation/ # Window animations
│ │ │ │ └── observer.rs # macOS accessibility observer
│ │ │ ├── wallpaper/ # Wallpaper management
│ │ │ ├── audio/ # Audio device manager
│ │ │ ├── hotkey/ # Global hotkey handling
│ │ │ ├── notunes/ # Prevents Apple Music auto-launch
│ │ │ └── cmd_q/ # Hold-to-quit (⌘Q) handler
│ │ └── 📁 ui/ # React frontend
│ │ ├── main.tsx # React app entry
│ │ ├── bar/ # Bar UI components (Media, Spaces, Status)
│ │ ├── hooks/ # React hooks (useTauriEventQuery, etc.)
│ │ └── design-system/ # Styling tokens (colors, motion)
│ └── 📁 shared/ # Shared Rust crate
│ └── 📁 src/
│ ├── lib.rs # Crate entry, re-exports
│ ├── config.rs # Config types
│ ├── schema.rs # JSON schema generation
│ └── tiling.rs # Shared tiling types
├── Cargo.toml # Workspace root
├── package.json # pnpm workspace root
└── vite.config.ts # Vite configuration

```

## Architecture

### CLI ↔ Desktop Communication

The CLI communicates with the running desktop app via Unix socket IPC:

```

CLI (barba reload) → Unix Socket → Desktop IPC Server → Handler → Response/Event

```

- CLI sends commands to `~/.local/run/barba.sock` (or `$XDG_RUNTIME_DIR/barba.sock`)
- Desktop's `ipc/` module listens and routes commands to appropriate handlers
- Some commands return JSON responses directly, others emit Tauri events to the frontend

### Tiling Window Manager

The built-in tiling window manager (`packages/desktop/tauri/src/tiling/`) provides:

- Multiple layout modes: tiling, monocle, master-stack, split, floating, scrolling
- Workspace management with per-screen workspaces
- Window rules for app-specific behavior
- Smooth animations for window transitions
- macOS Accessibility API integration for window control

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
├── ComponentName.types.ts    # TypeScript interfaces
└── ComponentName.test.tsx    # Component tests (Vitest)
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
- Events emitted via `app_handle.emit("event_name", payload)` or `window.emit()`
- IPC handlers in `packages/desktop/tauri/src/ipc/handlers/` - route CLI commands to appropriate modules
- Strict Clippy lints enabled: `pedantic`, `nursery`, `cargo` warnings (see workspace `Cargo.toml`)
- Uses Rust 2024 edition with latest stable toolchain

## CLI Commands

The standalone CLI (`barba`) provides comprehensive control over the desktop app:

```bash
# Configuration & Utilities
barba reload                              # Reload configuration
barba generate-schema                     # Output JSON schema for config
barba completions --shell <shell>         # Generate shell completions (bash, zsh, fish)

# Event Notifications (for external integrations)
barba focus-changed                       # Notify focus change
barba workspace-changed <name>            # Notify workspace change

# Query State (returns JSON)
barba query screens                       # List all connected screens
barba query workspaces [--name <name>|--focused|--focused-screen|--screen <screen>]
barba query windows [--focused-workspace|--focused-screen|--workspace <name>|--screen <screen>]

# Workspace Management
barba workspace focus <target>            # Focus workspace by name or direction (next, previous, up, down, left, right)
barba workspace layout <mode>             # Set layout: tiling, monocle, master, split, floating, scrolling
barba workspace send-to-screen <screen>   # Send workspace to another screen
barba workspace balance                   # Balance window sizes

# Window Management
barba window move <direction>             # Move/swap window (up, down, left, right)
barba window focus <direction>            # Focus window in direction (up, down, left, right, next, previous)
barba window send-to-workspace <name> [--focus=false]  # Send to workspace
barba window send-to-screen <screen>      # Send to screen
barba window resize <dimension> <amount>  # Resize width/height by pixels
barba window preset <name>                # Apply floating preset
barba window close                        # Close focused window

# Wallpaper Management
barba wallpaper set <path> [--screen <target>]  # Set specific wallpaper
barba wallpaper set --random [--screen <target>]  # Set random wallpaper
barba wallpaper generate-all              # Pre-generate all wallpapers
barba wallpaper list                      # List available wallpapers
```

## Development Commands

```bash
pnpm dev                  # Start Vite dev server (frontend only)
pnpm tauri:dev            # Full app with hot reload
pnpm tauri:build          # Build production app
pnpm build:cli            # Build CLI binary
pnpm test                 # Run all tests (UI + Rust)
pnpm test:ui              # Vitest browser tests
pnpm test:rust            # Rust tests via cargo-nextest
pnpm lint                 # ESLint/Stylelint + Clippy
pnpm lint:ui              # TypeScript check + ESLint + Stylelint
pnpm lint:rust            # Cargo sort + Clippy
pnpm format               # Prettier + cargo fmt
pnpm format:ui            # Prettier only
pnpm format:rust          # cargo fmt only
```

## Testing Conventions

- Frontend tests use Vitest with `vitest-browser-react` and Playwright for component testing
- Test files co-located: `ComponentName.test.tsx` alongside source
- Rust tests inline with `#[cfg(test)]` modules in the same file
- Run `pnpm test` to run all tests (UI + Rust)

## Path Aliases

- `@/` maps to `./packages/desktop/ui/` (configured in `vite.config.ts` and `tsconfig.app.json`)

## Critical Files

- `packages/desktop/tauri/src/lib.rs` - Tauri app entry, command registration, plugin setup
- `packages/desktop/tauri/src/ipc/mod.rs` - IPC server entry point for CLI communication
- `packages/desktop/tauri/src/ipc/server.rs` - Unix socket server implementation
- `packages/desktop/tauri/src/ipc/handlers/` - Command handlers for CLI requests
- `packages/desktop/tauri/src/tiling/mod.rs` - Tiling window manager entry point
- `packages/desktop/ui/hooks/useTauriEventQuery.ts` - Core pattern for Tauri-React integration
- `packages/cli/src/main.rs` - CLI entry point with Clap
- `packages/cli/src/commands.rs` - All CLI command definitions
- `packages/shared/src/config.rs` - Shared config types
- `packages/shared/src/schema.rs` - JSON schema generation
- `packages/shared/src/tiling.rs` - Shared tiling types
- `Cargo.toml` - Workspace root defining all Rust packages

## Additional Notes

- The app is macOS-only due to dependencies on macOS-specific APIs (Accessibility API for tiling, status bar integration, wallpaper management).
- The tiling window manager requires accessibility permissions to control window positioning.
- Uses Catppuccin Mocha color palette for UI styling (see `packages/desktop/ui/design-system/colors.ts`).
- Vite config uses `rolldown-vite` as the bundler for faster builds.
- Follow existing code patterns closely for consistency.
- After any iteration, run `pnpm lint` and `pnpm format` to ensure code quality.
