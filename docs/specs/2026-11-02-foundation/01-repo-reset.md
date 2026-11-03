---
status: done
---

# 01 — Archive and buildable skeleton

Read when resetting the repository without breaking the old application.

## Scope and files

Archive first; replace root tooling second. No real SDK behavior/module port yet.

## Requirements

- Move app/, old package/lock/workspace manifests, Cargo manifest/lock, schema, configs and scripts to
  `_legacy/`. Copy shared build inputs where both workspaces need them. Legacy owns package.json,
  pnpm-lock.yaml, pnpm-workspace.yaml, Cargo.toml/Cargo.lock, rust-toolchain.toml, rustfmt.toml,
  .config/nextest.toml and old frontend/lint/test configs. Preserve release/sidecar/resource inputs;
  repair paths/readme references. Legacy prepare must not replace root hooks.
- Move old docs except index/workflow/specs to `_legacy/docs/`; optional `_docs/` moves too. Preserve
  active agents/harnesses/specs/workflow/LICENSE/repository metadata. Write `_legacy/README.md` build
  instructions. Explicitly exclude archive from new Cargo workspace, watchers, globs and coverage.
- Create `src/crates/{sdk,macos,app,cli}/{Cargo.toml,src/lib.rs}`. App alone declares `stache` binary and
  adds src/main.rs, build.rs, tauri.conf.json, capabilities and icons. Preserve workspace lints/profile,
  nightly and root rustfmt.toml unchanged. Root Cargo.toml lists `src/crates/{app,cli,sdk,macos}`
  and later individual `src/crates/modules/<crate-id>` members, never the modules container;
  explicitly exclude `_legacy`. SDK has no Tauri dependency.
- Create `src/ui/src/sdk` and sibling `src/ui/src/shell/{components,modules}`. Shell lives in
  `src/ui/src/shell/components/Shell/{Shell.tsx,Shell.test.tsx,Shell.styles.tsx,index.ts}`; future
  surfaces use `src/ui/src/shell/modules/moduleName/` containing `moduleName.tsx`,
  `moduleName.test.tsx`, `moduleName.styles.tsx`, `index.ts` (no tray/bar or pilot UI modules now). Add
  `src/ui/src/utils/{utilName.ts,index.ts}` only for an actual shared utility, with colocated tests.
  Frontend root owns `src/ui/{index.html,index.tsx,main.tsx,main.css}`: main.tsx bootstraps, index.tsx
  exports/renders root, index.html is required by Vite. `src/ui/tests/setup.ts` is test setup;
  tests/ may hold support/fixtures/mocks, never suites. Actual browser smoke test is colocated. Retain React,
  TS, Linaria, React Query, zustand, React compiler, Vite and browser Vitest/WebKit with existing coverage
  thresholds (80/80/65/80). No widgets/unported features in new source/dependencies.
- Replace root package tooling with tested/pinned Bun and bun.lock. Scripts: build, tauri:dev,
  tauri:build, lint, lint:ui, lint:native, format, format:check, test, test:ui, test:native,
  test:ui:coverage. Lint/check commands never mutate files; formatting/fixes are explicit. Use
  installed dependency binaries directly; use `bun run` only to compose scripts. Remove root pnpm
  files/.markdownlint.jsonc.
- Update TS/Vite/Ox/stylelint configs, .gitignore, lint-staged.config.ts, scripts and CI for new paths.
  Configs/scripts/docs stay at repository root. Vite root is `src/ui`; TS includes frontend root
  entrypoints and `src/ui/src/**`. Repository test glob is `src/ui/src/**/*.test.{ts,tsx}` (Vitest
  root-relative `src/**/*.test.{ts,tsx}`); setup is `tests/setup.ts` relative to frontend root.
  Vite's default watcher stays rooted at `src/ui`, outside native/archive/root build inputs; new lint,
  format, hooks and coverage scopes exclude `_legacy/**` while covering the nested UI sources.
  Enable `.styles.tsx` in wyw include `src/ui/src/**/*.styles.tsx` and stylelint/lint-staged selectors;
  browser tests and frontend build prove the TSX style pipeline works, not old `.styles.ts` matching.
  Check pinned oxfmt Markdown support with a fixture. Oxlint remains JS/TS linting, not prose linting.
- Keep Husky: shell-based and package-manager-neutral, so replacement buys nothing. Root prepare
  installs hooks in a normal checkout without sidecar setup; `HUSKY=0` skips installation in CI.
  Linked-worktree prepare explicitly skips installation rather than modifying shared Git config or
  synthesizing local shims. If inherited hook config has no local shims, document direct
  `sh .husky/pre-commit` / `sh .husky/pre-push` invocation from that worktree.
  Pre-commit uses Bun lint-staged with archive/generated/worktree
  exclusions and workspace checks for staged Rust. Pre-push runs `bun run test`. Exercise in temp clone.
- Update .github/workflows/quality.yml: pinned Bun/frozen install, Rust, cargo-sort/nextest/audit,
  Playwright WebKit, lint/format/tests/coverage/audits, frontend/native builds on macOS 26+.
  Default jobs exclude legacy; separate opt-in instructions verify legacy.
- Tray-only app: Stache Next/next ID, minimumSystemVersion 26.0, Quit item, no configured windows,
  inherited media-control resources or killall of legacy stache. Minimal run() until host integration.

## Acceptance criteria

- [x] F01: Legacy independently installs/builds frontend/native and bundles with its own inputs;
      root Cargo/Bun never selects legacy. Old identity stays unchanged.
- [x] F02: Clean-install skeleton builds/passes lint/format/tests using Bun without weakening retained
      toolchain, lints, browser runner or coverage gates.
- [x] F03: New release bundle launches on macOS 26+, shows Quit tray item, creates zero webviews and
      neither overwrites nor terminates legacy.
- [x] F04: Hooks handle Rust/UI/Markdown/legacy-only staged changes correctly; CI runs equivalent
      non-mutating gates and frozen install.
- [x] F05: Markdownlint is removed from new tree; verified oxfmt formats/checks Markdown, with the
      absence of replacement semantic/prose linting documented honestly.
- [x] F06: Docs archive/routing is correct; active workflow/spec metadata stays usable; root README,
      AGENTS, workflow and role command examples use Bun.
- [x] F07: New source/manifests/capabilities contain no widgets, unported runtimes, notification control
      or legacy sidecar build requirements.

## Tests and validation

Keep actual app tests, not a custom test suite for installed tools or Git scaffolding. Bun is the
package/script runner, not a TypeScript test runner. Manually check formatter exclusions and Markdown
support, and exercise hooks in a disposable checkout; verify linked prepare skips without changing
shared config. Manually smoke Vite HMR and archive/nested-worktree isolation once, restoring edits.
Do not ship watcher modules, source-mutating test harnesses or a separate tooling Vitest config.
Native dev watching is a manual acceptance check: document exact edit/restore, ignored-path and
foreground-process cleanup commands, and record the result or outstanding gap. No persistent native
process-tree orchestration or minified-CSS build posthook is required.
Add colocated `Shell.test.tsx`.
Verify Vitest discovers colocated suites but none under `src/ui/tests/`, setup executes, and a
`Shell.styles.tsx` import passes wyw extraction, stylelint and release frontend build. Assert the
computed style in the browser and inspect generated CSS once, not through a brittle build posthook.
Include the TSX style path in manual hook checks. Run
`bun install --frozen-lockfile`, `bun run lint`, `bun run format:check`, `bun run test`,
`bun run test:ui:coverage`, `bun run build`, `bun run tauri:build`; manually launch/quit bundle.
Separately run `pnpm --dir _legacy install --frozen-lockfile` and legacy build/bundle commands from
`_legacy` (intentional pnpm exception). Record signing/resource prerequisites.

## Docs

Write docs/architecture/overview.md and docs/guides/testing.md for the shipped skeleton with links
to remaining planned parts. Update docs/index.md, README, AGENTS, workflow and .agents/roles/*.md.
Do not rewrite archived docs' historical pnpm instructions.
