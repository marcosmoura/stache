# Tiling Window Manager

Located in `app/native/src/modules/tiling/`.

## Key Components

- `manager/` — TilingManager singleton orchestrating all operations
- `workspace.rs` — Virtual desktops with rules-based window assignment
- `layout/` — Layout algorithms (dwindle, monocle, master, split, grid, floating)
- `borders/` — JankyBorders integration for window border rendering
- `observer.rs` — AXObserver for window events
- `rules.rs` — Window rule matching
- `animation.rs` — Window animation system

## Border Updates

Border colors are managed through JankyBorders:

```rust
// Update colors based on layout state
janky::update_colors_for_workspace(is_monocle, is_floating);

// Called after:
// - Window focus changes
// - Layout changes
// - Window creation/app launch
// - Startup (after layout determined)
```

## Adding a New Layout

1. Create `app/native/src/modules/tiling/layout/layout_name.rs`
2. Implement the calculate function:

   ```rust
   pub fn calculate(
       windows: &[&TrackedWindow],
       area: Rect,
       gaps: &Gaps
   ) -> LayoutResult
   ```

3. Add to `LayoutType` enum in `config/types.rs`
4. Add match arm in `layout/mod.rs` `calculate_layout()`
5. Regenerate schema: `./scripts/generate-schema.sh`

## Adding a New Tiling CLI Command

1. Add IPC notification constant to `platform/ipc.rs`
2. Add CLI command to `cli/commands.rs`
3. Send notification in `main.rs` command handler
4. Handle notification in `modules/bar/ipc_listener.rs`
5. Implement logic in `modules/tiling/manager/`
