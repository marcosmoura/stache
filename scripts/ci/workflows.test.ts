import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { afterEach, describe, expect, it } from 'vitest';

import { checkWorkflow, checkWorkflows, readWorkflows } from './workflows';

const SHA = 'fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09';

const valid = `name: Fixture

on:
  pull_request:

permissions:
  contents: read

jobs:
  build:
    runs-on: macos-26
    steps:
      - uses: actions/checkout@${SHA} # v5.1.0
        with:
          persist-credentials: false
      - uses: taiki-e/install-action@${SHA} # v2.90.3
        with:
          tool: cargo-sort@2.1.4,cargo-nextest@0.9.151
      - name: Lockfile matches manifests
        run: cargo fetch --locked
      - run: bun install --frozen-lockfile
      - run: bun run lint
      - name: Audit
        run: |
          bun audit
          cargo audit
      - run: git diff --exit-code -- Cargo.lock bun.lock
`;

function check(text: string): string[] {
  return checkWorkflow({ file: 'fixture.yml', text });
}

function lineOf(text: string, needle: string): number {
  return text.split('\n').findIndex((line) => line.includes(needle)) + 1;
}

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe('repository workflows', () => {
  it('follow the CI pin, token, credential, tool and lock-guard rules', () => {
    const files = readWorkflows(path.resolve(import.meta.dirname, '..', '..'));

    expect(files.length).toBeGreaterThan(0);
    expect(checkWorkflows(files)).toEqual([]);
  });
});

describe('checkWorkflow', () => {
  it('accepts a workflow written the way this repository writes them', () => {
    expect(check(valid)).toEqual([]);
  });

  it('rejects a tag pin', () => {
    const text = valid.replace(`actions/checkout@${SHA} # v5.1.0`, 'actions/checkout@v5 # v5.1.0');

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'actions/checkout@')}: uses "actions/checkout@v5" is not pinned to a 40-character commit SHA`,
    ]);
  });

  it('rejects a branch pin', () => {
    const text = valid.replace(`actions/checkout@${SHA}`, 'actions/checkout@main');

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'actions/checkout@')}: uses "actions/checkout@main" is not pinned to a 40-character commit SHA`,
    ]);
  });

  it('rejects a missing version comment', () => {
    const text = valid.replace(`install-action@${SHA} # v2.90.3`, `install-action@${SHA}`);

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'install-action@')}: uses "taiki-e/install-action@${SHA}" lacks a "# vX.Y.Z" version comment`,
    ]);
  });

  it('rejects missing permissions', () => {
    const text = valid.replace('permissions:\n  contents: read\n', '');

    expect(check(text)).toEqual(['fixture.yml:1: top-level "permissions:" block is missing']);
  });

  it('rejects an extra permission', () => {
    const text = valid.replace('  contents: read\n', '  contents: read\n  packages: write\n');

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'packages: write')}: top-level permissions grant "packages: write"; only "contents: read" is allowed`,
    ]);
  });

  it('accepts a job-level permissions block while the top-level block stays read-only', () => {
    const text = valid.replace(
      '    runs-on: macos-26\n',
      '    runs-on: macos-26\n    permissions:\n      contents: write\n',
    );

    expect(check(text)).toEqual([]);
    expect(check(text.replace('  contents: read\n', '  contents: write\n'))).toEqual([
      `fixture.yml:${lineOf(text, '  contents: read')}: top-level permissions grant "contents: write"; only "contents: read" is allowed`,
    ]);
  });

  it('rejects a flow-style step', () => {
    const text = valid.replace(
      '      - run: git diff',
      `      - { uses: actions/cache@${SHA} }\n      - {run: git status}\n      - run: git diff`,
    );

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, '- { uses')}: flow-style steps ("- { ... }") are not supported; write the step as a block`,
      `fixture.yml:${lineOf(text, '- {run')}: flow-style steps ("- { ... }") are not supported; write the step as a block`,
    ]);
  });

  it('rejects persisted checkout credentials', () => {
    const text = valid.replace(
      '          persist-credentials: false\n',
      '          fetch-depth: 0\n',
    );

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'actions/checkout@')}: checkout step lacks "persist-credentials: false"`,
    ]);
  });

  it('reads the credentials of a checkout step that starts with a name', () => {
    const named = valid.replace(
      `      - uses: actions/checkout@${SHA} # v5.1.0\n`,
      `      - name: Checkout\n        uses: actions/checkout@${SHA} # v5.1.0\n`,
    );
    const persisted = named.replace(
      '          persist-credentials: false\n',
      '          fetch-depth: 0\n',
    );

    expect(check(named)).toEqual([]);
    expect(check(persisted)).toEqual([
      `fixture.yml:${lineOf(persisted, 'actions/checkout@')}: checkout step lacks "persist-credentials: false"`,
    ]);
  });

  it('rejects an unpinned tool', () => {
    const text = valid.replace('cargo-nextest@0.9.151', 'cargo-nextest');

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'tool:')}: tool "cargo-nextest" lacks an exact @<version>`,
    ]);
  });

  it('rejects the lock guard after the first cargo-running step', () => {
    const text = valid
      .replace('      - name: Lockfile matches manifests\n        run: cargo fetch --locked\n', '')
      .replace(
        '      - run: bun run lint\n',
        '      - run: bun run lint\n      - run: cargo fetch --locked\n',
      );

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'bun run lint')}: cargo-running step comes before the "cargo fetch --locked" step on line ${lineOf(text, 'cargo fetch')}`,
    ]);
  });

  it('rejects a cargo-running workflow without the lock guard', () => {
    const text = valid.replace(
      '      - name: Lockfile matches manifests\n        run: cargo fetch --locked\n',
      '',
    );

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'bun run lint')}: no "cargo fetch --locked" step before this cargo-running step`,
    ]);
  });

  const unguarded = (step: string) =>
    valid
      .replace('      - name: Lockfile matches manifests\n        run: cargo fetch --locked\n', '')
      .replace('      - run: bun run lint\n', '')
      .replace(
        '      - name: Audit\n        run: |\n          bun audit\n          cargo audit\n',
        '',
      )
      .replace(
        '      - run: git diff',
        `${step}\n      - run: cargo fetch --locked\n      - run: git diff`,
      );

  it.each([
    ['a "|-" block scalar', '      - run: |-\n          cargo build', 'run: |-'],
    ['a ">+" block scalar', '      - run: >+\n          cargo build', 'run: >+'],
    ['bunx', '      - run: bunx tauri build', 'bunx'],
    ['npx', '      - run: npx tauri build', 'npx'],
    [
      'the Tauri CLI under node_modules',
      '      - run: ./node_modules/.bin/tauri build',
      'tauri build',
    ],
    ['a bun script', '      - run: bun test:scripts', 'bun test:scripts'],
    ['a double-quoted command', '      - run: "cargo build"', '"cargo build"'],
    ['a single-quoted command', "      - run: 'cargo build'", "'cargo build'"],
    ['a nested shell command', "      - run: sh -c 'cargo build'", 'sh -c'],
    ['cargo called by path', '      - run: ~/.cargo/bin/cargo build', 'bin/cargo build'],
    ['bun run', '      - run: bun run lint', 'bun run lint'],
  ])('rejects %s before the lock guard', (_name, step, needle) => {
    const text = unguarded(step);

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, needle)}: cargo-running step comes before the "cargo fetch --locked" step on line ${lineOf(text, 'cargo fetch')}`,
    ]);
  });

  it.each([
    'bun install --frozen-lockfile',
    'bun audit',
    'bun pm ls',
    'bun --version',
    'bun -v',
    'node_modules/.bin/playwright install webkit',
  ])('allows "%s" before the lock guard', (command) => {
    expect(check(unguarded(`      - run: ${command}`))).toEqual([]);
  });

  it('keeps the per-job guard when "jobs:" carries a comment', () => {
    const text = `${valid.replace('\njobs:\n', '\njobs: # every job\n')}
  test:
    runs-on: macos-26
    steps:
      - run: cargo build
`;

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'cargo build')}: no "cargo fetch --locked" step before this cargo-running step`,
    ]);
  });

  it('accepts a flow-style matrix entry, which is not a step', () => {
    const text = valid.replace(
      '    runs-on: macos-26\n',
      '    runs-on: macos-26\n    strategy:\n      matrix:\n        include:\n          - { target: aarch64 }\n',
    );

    expect(check(text)).toEqual([]);
  });

  it('requires the lock guard in every job that runs cargo', () => {
    const text = `${valid}
  test:
    runs-on: macos-26
    steps:
      - run: cargo build
`;

    expect(check(text)).toEqual([
      `fixture.yml:${lineOf(text, 'cargo build')}: no "cargo fetch --locked" step before this cargo-running step`,
    ]);
  });
});

describe('checkWorkflows', () => {
  it('checks every workflow file, naming the offending one', () => {
    const root = mkdtempSync(path.join(os.tmpdir(), 'stache-workflows-test-'));

    roots.push(root);
    mkdirSync(path.join(root, '.github', 'workflows'), { recursive: true });
    writeFileSync(path.join(root, '.github', 'workflows', 'quality.yml'), valid);
    writeFileSync(
      path.join(root, '.github', 'workflows', 'release.yml'),
      valid.replace(`actions/checkout@${SHA} # v5.1.0`, 'actions/checkout@v5'),
    );
    writeFileSync(
      path.join(root, '.github', 'workflows', 'nightly.yaml'),
      valid.replace('cargo-nextest@0.9.151', 'cargo-nextest'),
    );

    const errors = checkWorkflows(readWorkflows(root));
    const line = lineOf(valid, 'actions/checkout@');

    expect(errors).toEqual([
      `${path.join('.github', 'workflows', 'nightly.yaml')}:${lineOf(valid, 'tool:')}: tool "cargo-nextest" lacks an exact @<version>`,
      `${path.join('.github', 'workflows', 'release.yml')}:${line}: uses "actions/checkout@v5" is not pinned to a 40-character commit SHA`,
      `${path.join('.github', 'workflows', 'release.yml')}:${line}: uses "actions/checkout@v5" lacks a "# vX.Y.Z" version comment`,
    ]);
  });
});
