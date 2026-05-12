# Gotchas

Critical pitfalls and non-obvious requirements.

## Event Names Must Match

Event constants in Rust (`events.rs`) and TypeScript (`types/tauri-events.ts`) must be identical strings. Mismatches cause silent failures.

## Tauri Commands Need Registration

Every `#[tauri::command]` function must be added to `generate_handler![]` in `lib.rs` or it won't be callable from the frontend.

## Config Changes Need Schema Update

After modifying any struct in `config/types.rs`, run:

```bash
./scripts/generate-schema.sh
```

## Path Strings Need Expansion

User-provided paths may contain `~` or relative paths. Always use:

```rust
use crate::platform::path::{expand, expand_and_resolve};
```

Never use raw path strings from config directly.

## Strict Clippy Lints

The project uses `pedantic` and `nursery` Clippy lints. Run `cargo clippy` before committing.

## Coverage Thresholds

Tests must maintain: 80% lines/functions/statements, 65% branches.
