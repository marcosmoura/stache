# AGENTS.md - AI Agent Instructions for Barba Shell

This document provides instructions for AI coding agents (Claude, GPT, Copilot, Cursor, etc.) working on the Barba Shell codebase.

## Project Summary

**Barba Shell** is a macOS-only Tauri 2.x desktop application that provides:

- A custom status bar with Hyprspace tiling window manager integration
- Dynamic wallpaper management with effects (blur, rounded corners)
- Audio device auto-switching based on priority rules
- Media playback controls and display
- Global hotkey support
- "MenuAnywhere" - summon app menus at cursor position
- "noTunes" - prevent Apple Music from auto-launching

## Architecture Overview

```text
┌─────────────────────────────────────────────────────────────┐
│                    Barba Shell Binary                        │
│  ┌─────────────────────┐    ┌─────────────────────────────┐ │
│  │   CLI Mode          │    │      Desktop App Mode       │ │
│  │   (with args)       │    │      (no args)              │ │
│  │                     │    │                             │ │
│  │  barba reload       │───►│  ┌─────────────────────┐   │ │
│  │  barba wallpaper    │    │  │  IPC Listener       │   │ │
│  │  barba audio        │    │  │  (NSDistributed     │   │ │
│  │  barba event        │    │  │   NotificationCenter)│   │ │
│  └─────────────────────┘    │  └──────────┬──────────┘   │ │
│                              │             │              │ │
│                              │  ┌──────────▼──────────┐   │ │
│                              │  │   Tauri Backend     │   │ │
│                              │  │   (Rust)            │   │ │
│                              │  └──────────┬──────────┘   │ │
│                              │             │              │ │
│                              │  ┌──────────▼──────────┐   │ │
│                              │  │   React Frontend    │   │ │
│                              │  │   (TypeScript)      │   │ │
│                              │  └─────────────────────┘   │ │
│                              └─────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

## Directory Structure

```text
barba-shell/
├── app/
│   ├── native/                    # Rust backend
│   │   ├── src/
│   │   │   ├── audio/             # Audio device management
│   │   │   │   ├── device.rs      # Device matching logic
│   │   │   │   ├── list.rs        # List devices
│   │   │   │   └── priority.rs    # Auto-switch logic
│   │   │   ├── bar/               # Status bar
│   │   │   │   ├── components/    # Bar widgets (Rust side)
│   │   │   │   │   ├── battery.rs
│   │   │   │   │   ├── cpu.rs
│   │   │   │   │   ├── media.rs
│   │   │   │   │   ├── weather.rs
│   │   │   │   │   └── ...
│   │   │   │   ├── ipc_listener.rs  # CLI notification handler
│   │   │   │   └── menubar.rs       # Menu bar visibility
│   │   │   ├── cli/               # CLI commands
│   │   │   │   └── commands.rs    # Clap command definitions
│   │   │   ├── config/            # Configuration
│   │   │   │   ├── types.rs       # Config structs + schemars
│   │   │   │   ├── env.rs         # .env file parsing
│   │   │   │   └── watcher.rs     # Hot reload
│   │   │   ├── utils/             # Utilities
│   │   │   │   ├── ipc.rs         # NSDistributedNotification
│   │   │   │   ├── path.rs        # Shell path expansion
│   │   │   │   └── cache.rs       # Cache management
│   │   │   ├── wallpaper/         # Wallpaper management
│   │   │   │   ├── manager.rs     # Selection & cycling
│   │   │   │   ├── processing.rs  # Blur, corners
│   │   │   │   └── macos.rs       # macOS APIs
│   │   │   ├── events.rs          # Event name constants
│   │   │   ├── lib.rs             # Tauri app init
│   │   │   └── main.rs            # Entry point
│   │   ├── Cargo.toml
│   │   └── tauri.conf.json
│   └── ui/                        # React frontend
│       ├── components/            # Shared components
│       │   ├── Button/
│       │   ├── Icon/
│       │   ├── ScrollingLabel/
│       │   ├── Stack/
│       │   └── Surface/
│       ├── design-system/         # Design tokens
│       │   ├── colors.ts          # Catppuccin Mocha
│       │   └── motion.ts          # Animation constants
│       ├── hooks/                 # React hooks
│       │   ├── useTauriEvent.ts
│       │   ├── useTauriEventQuery.ts
│       │   ├── useStoreQuery.ts
│       │   └── ...
│       ├── renderer/              # Window renderers
│       │   ├── bar/               # Status bar UI
│       │   │   ├── Media/
│       │   │   ├── Spaces/
│       │   │   └── Status/
│       │   │       ├── Battery/
│       │   │       ├── Clock/
│       │   │       ├── Cpu/
│       │   │       ├── KeepAwake/
│       │   │       └── Weather/
│       │   └── widgets/           # Widget overlay
│       ├── stores/                # Zustand stores
│       ├── types/                 # TypeScript types
│       │   └── tauri-events.ts    # Event constants
│       └── utils/
├── docs/
│   ├── sample-config.jsonc        # Example configuration
│   └── sample.env                 # Example .env file
├── scripts/
│   └── generate-schema.sh         # JSON Schema generator
├── barba.schema.json              # Config JSON Schema
├── Cargo.toml                     # Workspace root
├── package.json
└── vite.config.ts
```

## Key Patterns

### 1. Event Communication

Events follow the naming convention: `barba://<module>/<event-name>`

**Rust (events.rs):**

```rust
pub mod media {
    pub const PLAYBACK_CHANGED: &str = "barba://media/playback-changed";
}
```

**TypeScript (tauri-events.ts):**

```typescript
export const MEDIA_PLAYBACK_CHANGED = 'barba://media/playback-changed';
```

**Emitting (Rust):**

```rust
app_handle.emit(events::media::PLAYBACK_CHANGED, &payload)?;
```

**Listening (React):**

```typescript
useTauriEvent<MediaPayload>(MEDIA_PLAYBACK_CHANGED, (event) => {
  // Handle event
});
```

### 2. Tauri Commands

**Define in Rust:**

```rust
#[tauri::command]
pub fn get_battery_info() -> Result<BatteryInfo, String> {
    // Implementation
}
```

**Register in lib.rs:**

```rust
tauri::generate_handler![
    bar::components::battery::get_battery_info,
]
```

**Call from TypeScript:**

```typescript
const info = await invoke<BatteryInfo>('get_battery_info');
```

### 3. useTauriEventQuery Hook

Combines initial fetch with event subscription:

```typescript
const { data, isLoading } = useTauriEventQuery<BatteryInfo>({
  eventName: BATTERY_STATE_CHANGED,
  initialFetch: () => invoke<BatteryInfo>('get_battery_info'),
  transformFn: (payload) => payload, // Optional transform
});
```

### 4. Component File Structure

```text
ComponentName/
├── index.ts                  # export { ComponentName } from './ComponentName';
├── ComponentName.tsx         # React component
├── ComponentName.styles.ts   # Linaria CSS
├── ComponentName.types.ts    # TypeScript interfaces (optional)
├── ComponentName.state.ts    # Business logic (optional)
└── ComponentName.test.tsx    # Tests
```

### 5. Styling with Linaria

```typescript
// ComponentName.styles.ts
import { css } from '@linaria/core';
import { colors, motion } from '@/design-system';

export const container = css`
  background: ${colors.surface0};
  border-radius: 8px;
  transition: all ${motion.duration} ${motion.easing};
`;

export const containerActive = css`
  background: ${colors.surface1};
`;
```

```tsx
// ComponentName.tsx
import { cx } from '@linaria/core';
import * as styles from './ComponentName.styles';

export function ComponentName({ active }: Props) {
  return <div className={cx(styles.container, active && styles.containerActive)}>{/* ... */}</div>;
}
```

### 6. Configuration Types

Config types use `schemars` for JSON Schema generation:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WeatherConfig {
    /// Path to .env file with API keys
    pub api_keys: String,
    /// Default location for weather
    pub default_location: String,
}
```

### 7. Path Handling

Use the centralized path utility for user-provided paths:

```rust
use crate::utils::path::{expand, expand_and_resolve};

// Tilde expansion only
let home_path = expand("~/.config/barba");

// Tilde expansion + relative path resolution
let env_path = expand_and_resolve(".env", config_dir);
```

## Common Tasks

### Adding a New Status Bar Widget

1. **Rust side** (`app/native/src/bar/components/`):
   - Create `widget_name.rs` with Tauri command
   - Register command in `lib.rs`
   - Add event constant to `events.rs`

2. **TypeScript side** (`app/ui/renderer/bar/Status/`):
   - Create `WidgetName/` directory with component files
   - Add event constant to `types/tauri-events.ts`
   - Use `useTauriEventQuery` for data fetching

### Adding a New CLI Command

1. Add command to `app/native/src/cli/commands.rs`:

   ```rust
   #[derive(Subcommand)]
   pub enum Commands {
       /// Description
       NewCommand {
           #[arg(short, long)]
           option: Option<String>,
       },
   }
   ```

2. Handle in `main.rs` match statement

### Adding a New Configuration Option

1. Add field to appropriate struct in `config/types.rs`
2. Regenerate schema: `./scripts/generate-schema.sh`
3. Update `docs/sample-config.jsonc`

## Testing

### Frontend Tests

```bash
pnpm test:ui          # Run all UI tests
pnpm test:ui -- --ui  # Run with Vitest UI
```

Tests use:

- Vitest + Playwright (WebKit browser)
- `vitest-browser-react` for component testing
- Tauri mocks in `app/ui/tests/setup.ts`

### Rust Tests

```bash
cargo test --package barba    # Run all Rust tests
cargo test --package barba -- test_name  # Run specific test
```

Tests are inline with `#[cfg(test)]` modules.

## Linting & Formatting

```bash
pnpm lint         # All linters
pnpm lint:ui      # TypeScript + ESLint + Stylelint
pnpm lint:rust    # Clippy (pedantic + nursery)
pnpm format       # Prettier + cargo fmt
```

## Important Constraints

1. **macOS-only** - Uses macOS-specific APIs extensively
2. **Single binary** - CLI and desktop app share the same binary
3. **Rust 2024 edition** - Uses latest stable Rust features
4. **React 19** - Uses new React features (use, Suspense)
5. **Strict Clippy** - `pedantic` and `nursery` lints enabled
6. **Coverage thresholds** - 80% lines/functions/statements, 65% branches

## Environment Setup

1. Install Rust (stable toolchain)
2. Install Node.js 20+ and pnpm
3. Install Xcode Command Line Tools
4. Run `pnpm install`
5. Run `pnpm tauri:dev` for development

## Useful Commands

```bash
# Development
pnpm tauri:dev           # Full app with hot reload
pnpm dev                 # Frontend only

# Building
pnpm tauri:build         # Production build
cargo build --release    # Rust binary only

# Testing
pnpm test                # All tests
cargo clippy             # Lint check

# Schema
./scripts/generate-schema.sh  # Regenerate JSON Schema
```

## Common Gotchas

1. **Event names must match** - Rust `events.rs` and TypeScript `tauri-events.ts`
2. **Tauri commands need registration** - Add to `generate_handler![]` in `lib.rs`
3. **Config changes need schema update** - Run `generate-schema.sh`
4. **Path strings need expansion** - Use `utils::path::expand()` for user paths
5. **Cross-window state** - Use `useStoreQuery` for state shared between windows
