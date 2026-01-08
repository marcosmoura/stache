# Development Guide

This guide covers setting up a development environment, building, testing, and contributing to Stache.

## Prerequisites

### Required Tools

| Tool      | Version               | Purpose             |
| --------- | --------------------- | ------------------- |
| Rust      | Stable (2024 edition) | Backend development |
| Node.js   | 20+                   | Frontend tooling    |
| pnpm      | 9+                    | Package manager     |
| Xcode CLT | Latest                | macOS development   |

### Installation

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install pnpm
npm install -g pnpm

# Install Xcode Command Line Tools
xcode-select --install
```

## Getting Started

### Clone and Setup

```bash
git clone https://github.com/marcosmoura/stache.git
cd stache
pnpm install
```

### Development Mode

```bash
# Full app with hot reload
pnpm tauri:dev

# Frontend only (faster iteration for UI changes)
pnpm dev
```

### Build for Production

```bash
pnpm tauri:build
```

The built app will be at `app/native/target/release/bundle/macos/Stache.app`.

## Project Structure

```text
stache/
├── app/
│   ├── native/               # Rust backend (Tauri)
│   │   ├── src/
│   │   │   ├── main.rs       # Entry point
│   │   │   ├── lib.rs        # Tauri initialization
│   │   │   ├── cli/          # CLI commands
│   │   │   ├── config/       # Configuration
│   │   │   ├── bar/          # Status bar
│   │   │   ├── wallpaper/    # Wallpapers
│   │   │   ├── audio/        # Audio
│   │   │   ├── hotkey/       # Keybindings
│   │   │   ├── menu_anywhere/# MenuAnywhere
│   │   │   ├── notunes/      # noTunes
│   │   │   ├── cmd_q/        # Hold-to-quit
│   │   │   ├── widgets/      # Widget window
│   │   │   └── utils/        # Utilities
│   │   ├── Cargo.toml
│   │   └── tauri.conf.json
│   │
│   └── ui/                   # React frontend
│       ├── components/       # Shared components
│       ├── design-system/    # Styling tokens
│       ├── hooks/            # React hooks
│       ├── renderer/         # Window renderers
│       ├── stores/           # Zustand stores
│       ├── types/            # TypeScript types
│       └── utils/            # Utilities
│
├── docs/                     # Documentation
├── scripts/                  # Build scripts
├── stache.schema.json        # Config JSON Schema
├── Cargo.toml                # Workspace root
├── package.json
└── vite.config.ts
```

## Available Scripts

### Development

| Command          | Description                           |
| ---------------- | ------------------------------------- |
| `pnpm dev`       | Start Vite dev server (frontend only) |
| `pnpm tauri:dev` | Full app with hot reload              |

### Building

| Command            | Description           |
| ------------------ | --------------------- |
| `pnpm build`       | Build frontend        |
| `pnpm tauri:build` | Build production app  |
| `pnpm build:cli`   | Build CLI binary only |

### Testing

| Command                | Description        |
| ---------------------- | ------------------ |
| `pnpm test`            | Run all tests      |
| `pnpm test:ui`         | Run frontend tests |
| `pnpm test:ui -- --ui` | Run with Vitest UI |
| `pnpm test:rust`       | Run Rust tests     |

### Linting & Formatting

| Command          | Description                     |
| ---------------- | ------------------------------- |
| `pnpm lint`      | Run all linters                 |
| `pnpm lint:ui`   | TypeScript + ESLint + Stylelint |
| `pnpm lint:rust` | Clippy (pedantic + nursery)     |
| `pnpm format`    | Format all code                 |

### Other

| Command                        | Description            |
| ------------------------------ | ---------------------- |
| `./scripts/generate-schema.sh` | Regenerate JSON Schema |

## Adding Features

### Adding a New Status Bar Widget

1. **Create Rust component** (`app/native/src/bar/components/widget_name.rs`):

```rust
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WidgetInfo {
    pub value: String,
}

#[tauri::command]
pub fn get_widget_info() -> Result<WidgetInfo, String> {
    Ok(WidgetInfo {
        value: "data".to_string(),
    })
}
```

1. **Register command** (`app/native/src/lib.rs`):

```rust
tauri::generate_handler![
    // ... existing commands
    bar::components::widget_name::get_widget_info,
]
```

1. **Add event constant** (`app/native/src/events.rs`):

```rust
pub mod widget_name {
    pub const STATE_CHANGED: &str = "stache://widget-name/state-changed";
}
```

1. **Create React component** (`app/ui/renderer/bar/Status/WidgetName/`):

```text
WidgetName/
├── index.ts
├── WidgetName.tsx
├── WidgetName.styles.ts
└── WidgetName.test.tsx
```

1. **Add TypeScript event** (`app/ui/types/tauri-events.ts`):

```typescript
export const WIDGET_NAME_STATE_CHANGED = 'stache://widget-name/state-changed';
```

### Adding a New CLI Command

1. **Define command** (`app/native/src/cli/commands.rs`):

```rust
#[derive(Subcommand)]
pub enum Commands {
    // ... existing commands

    /// Description of new command
    NewCommand {
        #[arg(short, long)]
        option: Option<String>,
    },
}
```

1. **Handle command** (`app/native/src/main.rs`):

```rust
match cli.command {
    // ... existing handlers
    Some(Commands::NewCommand { option }) => {
        // Implementation
    }
}
```

### Adding a New Configuration Option

1. **Add field** (`app/native/src/config/types.rs`):

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct NewFeatureConfig {
    /// Enable the feature
    pub enabled: bool,
    /// Feature option
    pub option: String,
}
```

1. **Add to root config**:

```rust
pub struct Config {
    // ... existing fields
    pub new_feature: NewFeatureConfig,
}
```

1. **Regenerate schema**:

```bash
./scripts/generate-schema.sh
```

1. **Update documentation** (`docs/configuration.md`)

## Code Style

### Rust

- Clippy with `pedantic` and `nursery` lints
- Use `rustfmt` for formatting
- Follow Rust API guidelines

### TypeScript

- ESLint with strict config
- Prettier for formatting
- Use functional components with hooks

### CSS

- Linaria for styling
- Stylelint for linting
- Use design system tokens

## Testing Guidelines

### Frontend Tests

Tests use Vitest with Playwright (WebKit browser).

```typescript
// Component.test.tsx
import { render, screen } from '@testing-library/react';
import { Component } from './Component';

describe('Component', () => {
  it('renders correctly', () => {
    render(<Component />);
    expect(screen.getByText('Expected text')).toBeInTheDocument();
  });
});
```

Tauri APIs are mocked in `app/ui/tests/setup.ts`.

### Rust Tests

Tests are inline with `#[cfg(test)]` modules.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function() {
        assert_eq!(function(), expected);
    }
}
```

### Coverage Requirements

| Metric     | Threshold |
| ---------- | --------- |
| Lines      | 80%       |
| Functions  | 80%       |
| Statements | 80%       |
| Branches   | 65%       |

## Debugging

### Rust Backend

```bash
# Build with debug info
cargo build

# Run with logging
RUST_LOG=debug pnpm tauri:dev
```

### Frontend

- React DevTools (install browser extension)
- Vite's built-in HMR and error overlay
- Console.log with conditional logging

### Common Issues

**Accessibility permissions not working:**

- Remove and re-add Stache in System Preferences
- Restart Stache

**Hot reload not working:**

- Check Vite terminal for errors
- Try restarting `pnpm tauri:dev`

**Rust compilation errors:**

- Run `cargo clean` and rebuild
- Check Rust toolchain version

## Contributing

### Workflow

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make changes
4. Run linting and tests (`pnpm lint && pnpm test`)
5. Commit changes (`git commit -m 'Add amazing feature'`)
6. Push to branch (`git push origin feature/amazing-feature`)
7. Open a Pull Request

### Commit Messages

Follow conventional commits:

- `feat:` New feature
- `fix:` Bug fix
- `docs:` Documentation
- `style:` Code style (formatting)
- `refactor:` Code refactoring
- `test:` Tests
- `chore:` Maintenance

### Pull Request Guidelines

- Keep PRs focused on a single change
- Include tests for new features
- Update documentation as needed
- Ensure CI passes

## IDE Setup

### VS Code

Recommended extensions:

- rust-analyzer
- Tauri
- ESLint
- Prettier
- Stylelint

Settings are in `.vscode/settings.json`.

### Other IDEs

- **WebStorm/IntelliJ:** Install Rust plugin
- **Zed:** Configuration in `.zed/settings.json`

## Release Process

1. Update version in `package.json` and `Cargo.toml`
2. Update CHANGELOG
3. Create git tag: `git tag v1.0.0`
4. Push tag: `git push origin v1.0.0`
5. GitHub Actions builds and creates release

## Getting Help

- Check existing [GitHub Issues](https://github.com/marcosmoura/stache/issues)
- Read the [Architecture](./architecture.md) document
- Review the [AGENTS.md](../AGENTS.md) for AI agent instructions
