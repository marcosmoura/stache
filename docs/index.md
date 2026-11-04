# Stache Next docs

Read when navigating the shipped foundation, explicitly planned rewrites or the archived predecessor.

| Task                                | Read                                                                                        |
| ----------------------------------- | ------------------------------------------------------------------------------------------- |
| New tree and boundaries             | [architecture/overview.md](architecture/overview.md)                                        |
| Module SDK, lifecycle and threads   | [architecture/sdk.md](architecture/sdk.md)                                                  |
| Socket control and CLI              | [architecture/ipc.md](architecture/ipc.md)                                                  |
| Typed config, reload, schemas, CLI  | [architecture/config.md](architecture/config.md)                                            |
| Typed UI SDK, bindings, test window | [architecture/ui.md](architecture/ui.md)                                                    |
| Audio device switching (proxyAudio) | [features/proxy-audio.md](features/proxy-audio.md)                                          |
| Adding a module                     | [guides/new-module.md](guides/new-module.md)                                                |
| Build, checks, hooks, coverage      | [guides/testing.md](guides/testing.md)                                                      |
| Measure release startup and idle    | [guides/performance.md](guides/performance.md)                                              |
| Specs, worktrees and agents         | [workflow.md](workflow.md), [specs/README.md](specs/README.md)                              |
| Foundation spec and acceptance      | [spec](specs/2026-11-02-foundation/00-overview.md)                                          |
| Planned rewrite order               | [roadmap](roadmap.md)                                                                       |
| noTunes (shipped)                   | [feature](features/notunes.md), [spec](specs/2026-11-03-notunes/00-overview.md)             |
| Keep Awake (shipped)                | [feature](features/keep-awake.md), [spec](specs/2026-11-03-keep-awake/00-overview.md)       |
| Wallpapers (planned)                | [feature](features/wallpapers.md), [spec](specs/2026-11-03-wallpapers/00-overview.md)       |
| Keybindings (planned)               | [feature](features/keybindings.md), [spec](specs/2026-11-03-keybindings/00-overview.md)     |
| Hold-to-Quit (planned)              | [feature](features/hold-to-quit.md), [spec](specs/2026-11-03-hold-to-quit/00-overview.md)   |
| MenuAnywhere (planned)              | [feature](features/menu-anywhere.md), [spec](specs/2026-11-03-menu-anywhere/00-overview.md) |
| Bar (planned)                       | [feature](features/bar.md), [spec](specs/2026-11-03-bar/00-overview.md)                     |
| Tiling (planned)                    | [feature](features/tiling.md), [spec](specs/2026-11-03-tiling/00-overview.md)               |
| Coordinated teardown                | [spec](specs/2026-11-03-teardown/00-overview.md)                                            |
| Release/packaging (planned)         | [spec](specs/2026-11-03-release/00-overview.md)                                             |
| Old application/features            | [legacy docs](../_legacy/docs/index.md), [legacy build](../_legacy/README.md)               |
| Tray menu and actions (shipped)     | [features/tray.md](features/tray.md)                                                        |

## Doc style

- Start with a title and “Read when…” line.
- Describe shipped facts, paths, invariants and pitfalls; label plans as plans.
- Link to symbols instead of copying code. Aim for about 150 lines per doc.
- Archive historical behavior docs; keep workflow/spec metadata active.
