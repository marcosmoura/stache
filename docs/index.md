# Stache docs

Read this file, then only the docs that match your task.

## Routing

| If you are touching…                                             | Read                                                           |
| ---------------------------------------------------------------- | -------------------------------------------------------------- |
| Anything, for the first time                                     | [architecture/overview.md](architecture/overview.md)           |
| CLI commands, CLI ↔ app messaging, Tauri commands or events      | [architecture/ipc.md](architecture/ipc.md)                     |
| Config types, config loading, hot reload, `stache.schema.json`   | [architecture/config.md](architecture/config.md)               |
| Rust code in `app/native/`                                       | [guides/rust.md](guides/rust.md)                               |
| React code in `app/ui/`                                          | [guides/react.md](guides/react.md)                             |
| Writing or running tests                                         | [guides/testing.md](guides/testing.md)                         |
| A common change (new widget, CLI command, config option, layout) | [guides/recipes.md](guides/recipes.md)                         |
| Anything non-obvious that has bitten before                      | [gotchas.md](gotchas.md)                                       |
| Agent workflow, specs, worktrees                                 | [workflow.md](workflow.md), [specs/README.md](specs/README.md) |

## Features

| Feature                                     | Doc                                                                              | Code                                               |
| ------------------------------------------- | -------------------------------------------------------------------------------- | -------------------------------------------------- |
| Status bar and its widgets                  | [features/bar.md](features/bar.md)                                               | `modules/bar/`, `app/ui/renderer/bar/`             |
| Widget overlays                             | [features/widgets.md](features/widgets.md)                                       | `modules/widgets/`, `app/ui/renderer/widgets/`     |
| Tiling window manager                       | [features/tiling.md](features/tiling.md) (start here)                            | `modules/tiling/`                                  |
| ↳ Tiling: observers, identity, rules, tabs  | [features/tiling/events-and-identity.md](features/tiling/events-and-identity.md) | `modules/tiling/events/`, `identity.rs`, `rules/`  |
| ↳ Tiling: layouts, gaps, animation, borders | [features/tiling/layout-and-effects.md](features/tiling/layout-and-effects.md)   | `modules/tiling/layout/`, `effects/`, `borders.rs` |
| Wallpapers                                  | [features/wallpapers.md](features/wallpapers.md)                                 | `modules/wallpaper/`                               |
| Audio device switching                      | [features/audio.md](features/audio.md)                                           | `modules/audio/`                                   |
| Keybindings and Caps Lock remap             | [features/keybindings.md](features/keybindings.md)                               | `modules/hotkey/`                                  |
| MenuAnywhere                                | [features/menu-anywhere.md](features/menu-anywhere.md)                           | `modules/menu_anywhere/`                           |
| noTunes                                     | [features/notunes.md](features/notunes.md)                                       | `modules/notunes/`                                 |
| Hold-to-Quit                                | [features/hold-to-quit.md](features/hold-to-quit.md)                             | `modules/cmd_q/`                                   |
| Keep Awake                                  | [features/keep-awake.md](features/keep-awake.md)                                 | `modules/bar/components/keepawake.rs`              |
| Tray icon                                   | [features/tray.md](features/tray.md)                                             | `modules/tray/`                                    |

Rust paths are relative to `app/native/src/`.

Sample user files: [samples/config.jsonc](samples/config.jsonc), [samples/.env.example](samples/.env.example).

## Doc style

These rules apply to every doc in this folder.

- The first line after the title says when to read the doc ("Read when…").
- State facts, file paths, invariants and pitfalls. Don't write tutorials or restate what the code makes obvious.
- Point to code by path and symbol instead of pasting large snippets. Keep snippets to a few lines.
- Keep each doc under about 150 lines. Split it when it grows past that.
- A feature doc covers: purpose, entry points, key types, data flow, config, invariants and pitfalls, tests.
- Cross-cutting pitfalls go in `gotchas.md`. Pitfalls specific to one feature go in that feature's doc.
