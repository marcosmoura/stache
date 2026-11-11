---
status: draft
---

# 01 — CI hardening

## Scope

Harden `.github/workflows/quality.yml` against a compromised or moved action tag, a
write-capable token, persisted checkout credentials, silent lockfile drift and unpinned cargo
tools (CI-001..004). This also settles the unverified default `GITHUB_TOKEN` scope by declaring
it explicitly. Add the pin-update path chosen in Decision 5 and a test that keeps the rules
true. No product code changes.

## Requirements

- **SHA pins (CI-001).** Every `uses:` in every workflow under `.github/workflows/` references
  a full 40-character commit SHA followed by a `# vX.Y.Z` comment naming the exact release the
  SHA belongs to: `actions/checkout`, `oven-sh/setup-bun`, `actions/setup-node`,
  `Swatinem/rust-cache`, `taiki-e/install-action`. Resolve each SHA from the action's own
  repository: the commit a release tag points to, dereferencing annotated tags. Use the newest
  release within the major the workflow already uses. Record the lookup commands in the commit
  message body, not in the repo.
- **Token scope (CI-002).** A top-level `permissions:` block grants `contents: read` and nothing
  else. No job widens it. Check that no step needs more: `rust-cache` uses the Actions cache
  service, not `GITHUB_TOKEN` writes.
- **Checkout credentials (CI-002).** `actions/checkout` sets `persist-credentials: false`.
  No later step uses git credentials.
- **Lockfile guard (CI-003, Decision 4).** Right after the toolchain and cache steps and before
  any step that runs cargo (`check:generated` is the first), a step runs `cargo fetch --locked`.
  It fails when `Cargo.lock` does not match the manifests. The last step of the job runs
  `git diff --exit-code -- Cargo.lock bun.lock`. `package.json` scripts do not change. If the
  user picks the alternative instead: add `--locked` to `lint:native` (clippy), `test:native`,
  `generate-bindings` and `generate-schema`, use `tauri build ... -- --locked` for
  `tauri:build`, drop the guard step, and update `docs/guides/testing.md` to explain the local
  effect.
- **Pinned cargo tools (CI-004).** `taiki-e/install-action` installs
  `cargo-sort@<x.y.z>,cargo-nextest@<x.y.z>,cargo-audit@<x.y.z>`, each pinned to the current
  release, which the worker looks up. `docs/guides/testing.md` lists the same versions for
  local setup.
- **Update path (Decision 5).** `.github/dependabot.yml` with one `github-actions` ecosystem
  entry for `/`, a weekly schedule and one group that covers all actions. Do not add Cargo or
  Bun ecosystems. Dependabot must not need a token beyond its default. If the user picks the
  alternative: no file, and a "Refreshing CI pins" paragraph in `docs/guides/testing.md` with
  the lookup procedure.
- **Pin test.** `scripts/ci/workflows.test.ts` runs in `bun run test:scripts`. It reads every
  `.github/workflows/*.yml` as text and parses no YAML, so it adds no dependency. It fails when:
  - a `uses:` line lacks a 40-hex SHA or the version comment;
  - a `uses:` value is a branch or a tag;
  - the top-level `permissions:` block is missing or grants anything but `contents: read`;
  - `persist-credentials: false` is missing from a checkout step;
  - a `tool:` entry lacks `@<version>`;
  - the `cargo fetch --locked` step does not come before the first cargo-running step.
    It must point at the offending file and line. Keep it line-based and small. It enforces the
    rules for files written the way this repo writes them; it is not a general YAML validator.
- **Docs.** `docs/guides/testing.md` states the CI rules: SHA pins and how they are updated,
  read-only token, lock guard, tool versions.

## Acceptance criteria

- [ ] Every `uses:` in `.github/workflows/` is a 40-hex SHA with a matching `# vX.Y.Z` comment,
      and each SHA was resolved from the action's repository (listed in the commit body).
- [ ] `quality.yml` has top-level `permissions: contents: read` and checkout uses
      `persist-credentials: false`.
- [ ] `cargo fetch --locked` runs before the first cargo-running step, and the job ends with the
      lockfile diff check. Or, under the alternative, every cargo-running script is locked.
- [ ] `cargo-sort`, `cargo-nextest` and `cargo-audit` are pinned to explicit versions in CI and
      in `docs/guides/testing.md`.
- [ ] `.github/dependabot.yml` exists as specified, or the manual-refresh paragraph exists,
      per Decision 5.
- [ ] `scripts/ci/workflows.test.ts` passes on the new workflow. Each negative fixture (tag
      pin, missing comment, missing permissions, extra permission, persisted credentials,
      unpinned tool, guard after the first cargo step) fails with file and line.
- [ ] `bun run lint`, `bun run format:check` and `bun run test` pass locally.
- [ ] After the user allows a push, the first Quality run of the branch passes. Until then this
      criterion is recorded as not run and blocks `done`.

## Tests

- `scripts/ci/workflows.test.ts`: positive case against the real workflows. Negative cases use
  inline fixture strings for each rule above, plus a multi-workflow case so a second workflow
  file, such as the release spec's future `release.yml`, is covered automatically.
- End-to-end: the real Quality run on the pushed branch is the scenario. It proves the pinned
  actions resolve, the read-only token suffices for every step, the lock guard passes on a
  clean tree and the pinned tools install. Push only with the user's go-ahead. Do not simulate
  it.

## Notes

- `HUSKY: '0'` and `--frozen-lockfile` stay as they are.
- `cargo fetch --locked` warms the same registry cache `rust-cache` restores, so the extra cost
  is small. If it proves slow, `cargo metadata --locked --format-version 1 > /dev/null` is an
  acceptable substitute with the same failure mode. Record which one was used.
- Release Part 03's planned `release.yml` is covered by the pin test as soon as it exists.
  That is intentional, and that spec's owner should expect it.
