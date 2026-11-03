# Validation and measurements

Read when reviewing foundation quality gates; planned testing/performance guide content.

Parts materialize docs/guides/testing.md and performance.md, with exact commands after scripts exist.

## Quality gates

New tree uses Bun scripts, not Bun test runner. Vitest runs in real Playwright WebKit; native tests
use cargo-nextest. Rust retains pinned nightly/rustfmt and workspace Clippy pedantic/nursery policy,
warnings as errors, cargo-sort. Root checks exclude `_legacy/`; legacy builds independently with pnpm.

Root Cargo.toml lists `src/crates/{app,cli,sdk,macos}` and individual module crate paths as added,
not the modules container;
new frontend source is `src/ui/`. Root configs/scripts/docs stay root. Vite root is src/ui;
Vite's default watcher is rooted at src/ui, outside native/archive/root build inputs. TS/lint/hook scopes
cover frontend root entrypoints plus nested src, not historical legacy paths.

Repository suite glob: `src/ui/src/**/*.test.{ts,tsx}`; Vitest frontend-root-relative include:
`src/**/*.test.{ts,tsx}`, setup `tests/setup.ts`. Actual suites colocate with components/modules/client/
utils. `src/ui/tests/` holds only setup/fixtures/mocks, excluded from suite and source-coverage globs.
Part 1 validates `.styles.tsx` wyw extraction/stylelint/build and hooks select the TSX style file;
new component/module directories use named `.tsx`, `.test.tsx`, `.styles.tsx` and index.ts siblings.

Lint/format checks never rewrite. Hooks use Bun/lint-staged and Husky without legacy hook installation.
Keep app tests and typecheck root configs; no separate tooling test framework. Check hook routing
manually in a disposable checkout, without commits or identity changes.
Normal checkout prepare installs Husky; CI sets HUSKY=0. Linked prepare explicitly skips shared-config
changes and shim generation; document direct hook invocation where inherited config lacks local shims.
Vite HMR is a one-time manual smoke, with source edits restored. Native dev-watch
acceptance is manual with exact edit/restore/cleanup commands and recorded results or explicit gaps;
do not ship a process-tree test orchestrator. Prove TSX extraction through browser computed styles
and one release CSS inspection, not a minified-CSS build posthook.
Oxfmt formats supported Markdown; oxlint supports JS/TS/framework script blocks, not Markdown prose.
No Markdown semantic lint replacement is promised. Verify pinned tool behavior and document gaps.

Schema/binding generation --check writes temp output, compares committed artifacts and fails on drift.
Generated client path is `src/ui/src/sdk/bindings.gen.ts`.
Build/lint/CI must include both once introduced. Test a deliberately changed generated file to prove gate.
Use temporary config/runtime paths; fake lifecycle, permission and native services. CI never prompts
for Accessibility or switches actual device defaults.

## Measurement checklist

- Release builds, named machine/chip/RAM/macOS/commit/toolchain/profile/config/hardware/power state.
- Five launches minimum, host main→tray and external launch→tray acknowledgement reported separately.
- Warmup then 60 seconds idle CPU/RSS for host and attributable process tree, including WebKit/GPU/
  network; retain raw samples/tree, explain shared-page RSS counting and ambiguous attribution.
- Equivalent automation only: disable other legacy configurable tools; disclose unavoidable overhead.
- Sequential runs only; refuse other active automation, wait for all children to stop, restore audio
  defaults/config on success or error. Never run both legacy/new proxyAudio listeners in hardware tests.
- Instrument webview count; verify zero with pilot enabled/disabled and no window request.

Targets ≈0% idle CPU, lower tree RSS, host tray-ready <300 ms and zero webviews are provisional until
measured. Record noise/precision, medians/ranges and actual misses. Get explicit budget decision for
misses; do not fake pass numbers, silently relax gates or confuse process-launch time with host time.
