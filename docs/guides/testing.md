# Testing

Read when you write, run or debug tests.

## Commands

| Task             | Command                                                                    |
| ---------------- | -------------------------------------------------------------------------- |
| Everything       | `pnpm test` (`test:ui` then `test:native`)                                 |
| UI               | `pnpm test:ui` (`vitest run`)                                              |
| UI with coverage | `pnpm test:ui:coverage`                                                    |
| Rust             | `pnpm test:native` (`cargo nextest run --workspace`)                       |
| One UI file      | `pnpm exec vitest run app/ui/renderer/bar/Status/Battery/Battery.test.tsx` |
| One UI test      | add `-t "name of the test"` to the command above                           |
| One Rust test    | `cargo nextest run -p stache -E 'test(test_name)'`                         |
| One Rust module  | `cargo nextest run -p stache modules::tiling::layout`                      |

`.config/nextest.toml` keeps artifacts in `target/nextest`, sets `fail-fast = true` for the
default profile and `fail-fast = false` for the `ci` profile.

## Rust tests

- Tests live inline in `#[cfg(test)] mod tests` at the bottom of
  the file under test. There is no `app/native/tests/` directory and no shared test crate.
- Pure logic is tested exhaustively: `modules/tiling/layout` (routing, gaps, ratios),
  `modules/tiling/actor/handlers/resize`, the `app_shutdown` arbiter state machine,
  `modules/lifecycle_registry` (with a `MockModule` and a deliberately blocking pause).
- OS-facing helpers are tested only for "does not crash" where the result depends on the
  machine (`platform/accessibility.rs`, parts of `platform/objc.rs`).
- Concurrency tests use local atomics, channels and barriers instead of the global statics,
  which keeps them deterministic.
- The tiling runtime factory (`modules/tiling/init.rs`, `RuntimeFactory`) accepts an
  `InitStage` to inject a failure at a named stage; use it for staged-startup tests rather
  than mocking OS calls.

## UI tests

- Vitest browser mode with Playwright WebKit, headless, 800×600. Configuration lives in the
  `test` block of `vite.config.ts`; the include glob is `./**/*.test.{ts,tsx}` relative to
  `app/ui`, so a test file outside `app/ui` is never picked up.
- Tests sit next to the component (`Component.test.tsx`); state hooks get
  `*.state.test.ts(x)`.
- `app/ui/tests/setup.ts` installs a fake `window.__TAURI_INTERNALS__` with
  `defaultInvokeMocks` for the common commands (`get_battery_info`, `get_cpu_info`,
  `get_wifi_info`, `get_weather_config`, the tiling queries, …), event listener bookkeeping
  and a stubbed `@hugeicons/react`. Unknown commands resolve to `null`, so a new command
  usually needs an entry there before a component test can rely on it.
- `app/ui/tests/utils.tsx` provides `createTestQueryClient` (no retries, `gcTime: 0`,
  `staleTime: Infinity`) and `createQueryClientWrapper`. Component tests mock the store hook
  with `vi.mock('@/stores/…')` instead of exercising IPC.
- Coverage is istanbul with 80% lines/functions/statements and 65% branches. Excluded:
  `*.test.*`, `*.styles.ts`, `*.state.ts`, `main.tsx`, `tests/**`, `vite-env.d.ts`.

## Git hooks

- `.husky/pre-commit`: `cargo sort --workspace`, `cargo fmt --all`, clippy with `--fix` and
  `-D warnings`, then `lint-staged`. lint-staged runs oxfmt on js/ts/tsx/json/jsonc/css/md/
  yaml, and oxlint/stylelint with `--fix` on their globs. oxfmt skips `app/native/`.
- `.husky/pre-push`: `pnpm test`.

## CI

`.github/workflows/quality.yml`, macOS runner, runs in order: `tsc --noEmit`, `oxlint`,
`stylelint`, `oxfmt --check`, clippy `-D warnings`, `cargo fmt --check`, UI tests, native
tests, UI coverage, `pnpm audit --prod` plus `cargo audit --target-os macos`, then
`pnpm build` and `cargo build --workspace`. Playwright WebKit is installed explicitly before
the test steps.
