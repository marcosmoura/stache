import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, describe, expect, it } from 'vitest';

import type { WorkflowRun } from './gate';
import type { RunResult } from './pipeline';
import { limitations, releaseNotes, runPublish } from './publish';

const head = 'c'.repeat(40);
const fingerprint = 'AB'.repeat(32);
const dmg = 'Stache_0.26.0_aarch64.dmg';
const zip = 'Stache_0.26.0_aarch64.zip';
const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

function makeRoot(record: Record<string, unknown> = {}, skip: string[] = []): string {
  const root = mkdtempSync(join(tmpdir(), 'stache-publish-'));
  const output = join(root, 'target/release-artifacts');

  roots.push(root);
  mkdirSync(join(root, 'src/crates/app'), { recursive: true });
  mkdirSync(output, { recursive: true });
  writeFileSync(
    join(root, 'src/crates/app/tauri.conf.json'),
    JSON.stringify({
      productName: 'Stache',
      version: '0.26.0',
      identifier: 'com.marcosmoura.stache',
    }),
  );
  writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "0.26.0"\n');
  const files: Record<string, string> = {
    [dmg]: 'dmg',
    [zip]: 'zip',
    SHA256SUMS: 'sums\n',
    'release.json': JSON.stringify({
      version: '0.26.0',
      commit: head,
      treeClean: true,
      signing: 'self-signed',
      notarized: false,
      certificateSha256: fingerprint,
      artifacts: [dmg, zip],
      ...record,
    }),
  };

  for (const [name, content] of Object.entries(files)) {
    if (!skip.includes(name)) {
      writeFileSync(join(output, name), content);
    }
  }

  return root;
}

interface Fake {
  status?: string;
  shasum?: number;
  tags?: string;
  remote?: string;
  runs?: Partial<WorkflowRun>[];
  view?: RunResult;
}

const goodRun: WorkflowRun = {
  databaseId: 9,
  headSha: head,
  event: 'push',
  status: 'completed',
  conclusion: 'success',
  createdAt: '2026-11-20T10:00:00Z',
};

function publish(root: string, fake: Fake = {}, tag?: string) {
  const calls: { command: string; args: string[] }[] = [];
  const lines: string[] = [];
  let notes = '';
  const ok = (stdout = ''): RunResult => ({ status: 0, stdout, stderr: '' });
  const run = (command: string, args: string[]): RunResult => {
    calls.push({ command, args });
    const joined = args.join(' ');

    if (command === 'git' && joined.includes('rev-parse')) {
      return ok(`${head}\n`);
    }

    if (command === 'git' && joined.includes('status')) {
      return ok(fake.status ?? '');
    }

    if (command === 'shasum') {
      return { status: fake.shasum ?? 0, stdout: '', stderr: '' };
    }

    if (command === 'git' && joined.includes('for-each-ref')) {
      return ok(fake.tags ?? 'v0.26.0\n');
    }

    if (command === 'git' && joined.includes('ls-remote')) {
      const name = args.at(-1) ?? '';

      return ok(fake.remote ?? `${head}\t${name}\n`);
    }

    if (command === 'gh' && args[0] === 'run') {
      return ok(JSON.stringify((fake.runs ?? [goodRun]).map((r) => ({ ...goodRun, ...r }))));
    }

    if (command === 'gh' && joined.startsWith('release view')) {
      return fake.view ?? { status: 1, stdout: '', stderr: 'release not found\n' };
    }

    if (command === 'gh' && joined.startsWith('release create')) {
      notes = readFileSync(args[args.indexOf('--notes-file') + 1], 'utf8');

      return ok(`https://github.com/owner/repo/releases/tag/untagged-1\n`);
    }

    return { status: 1, stdout: '', stderr: `unexpected ${command} ${joined}` };
  };
  const result = () => runPublish({ root, run, log: (line) => lines.push(line), tag });

  return { calls, lines, result, notes: () => notes };
}

const creates = (calls: { command: string; args: string[] }[]) =>
  calls.filter(
    (call) => call.command === 'gh' && call.args[0] === 'release' && call.args[1] === 'create',
  );

describe('RELEASE-06-10 publish refusals', () => {
  it.each<[string, () => string, Fake, RegExp, string?]>([
    [
      'a missing release.json',
      () => makeRoot({}, ['release.json']),
      {},
      /release\.json is missing/,
    ],
    ['an ad-hoc record', () => makeRoot({ signing: 'ad-hoc' }), {}, /not "self-signed"/],
    [
      'a record without a fingerprint',
      () => makeRoot({ certificateSha256: undefined }),
      {},
      /no certificateSha256/,
    ],
    ['a dirty recorded tree', () => makeRoot({ treeClean: false }), {}, /dirty working tree/],
    ['another commit', () => makeRoot({ commit: 'd'.repeat(40) }), {}, /but HEAD is/],
    ['a dirty tree now', () => makeRoot(), { status: ' M README.md\n' }, /not clean/],
    ['a missing artifact', () => makeRoot({}, [zip]), {}, /missing artifacts.*aarch64\.zip/],
    ['failing checksums', () => makeRoot(), { shasum: 1 }, /shasum -a 256 -c SHA256SUMS failed/],
    ['no tag on HEAD', () => makeRoot(), { tags: '' }, /carries no tag v0\.26\.0/],
    ['a tag for another version', () => makeRoot(), { tags: 'v0.25.0\n' }, /carries no tag/],
    [
      'several matching tags',
      () => makeRoot(),
      { tags: 'v0.26.0-rc.1\nv0.26.0\n' },
      /several matching tags.*--tag/,
    ],
    ['a named tag not on HEAD', () => makeRoot(), {}, /does not point at HEAD/, 'v0.26.0-rc.2'],
    ['a named tag of another version', () => makeRoot(), {}, /does not match/, 'v0.27.0'],
    ['a tag missing on origin', () => makeRoot(), { remote: '' }, /not on origin/],
    [
      'a tag on origin at another commit',
      () => makeRoot(),
      { remote: `${'e'.repeat(40)}\trefs/tags/v0.26.0\n` },
      /on origin points at e+/,
    ],
    [
      'an annotated tag whose peeled commit differs',
      () => makeRoot(),
      {
        remote: `${head}\trefs/tags/v0.26.0\n${'e'.repeat(40)}\trefs/tags/v0.26.0^{}\n`,
      },
      /on origin points at e+/,
    ],
    ['no Quality run', () => makeRoot(), { runs: [] }, /no Quality run/],
    [
      'a pending Quality run',
      () => makeRoot(),
      { runs: [{ status: 'in_progress', conclusion: null }] },
      /in_progress; wait .* run this again/,
    ],
    [
      'a failed Quality run',
      () => makeRoot(),
      { runs: [{ conclusion: 'failure' }] },
      /concluded failure/,
    ],
    [
      'a pull-request-only Quality run',
      () => makeRoot(),
      { runs: [{ event: 'pull_request' }] },
      /no Quality run/,
    ],
    [
      'an existing release',
      () => makeRoot(),
      { view: { status: 0, stdout: '{}', stderr: '' } },
      /already exists/,
    ],
    [
      'another gh release view failure',
      () => makeRoot(),
      { view: { status: 1, stdout: '', stderr: 'HTTP 401: Bad credentials' } },
      /gh release view v0\.26\.0 failed: HTTP 401/,
    ],
  ])('refuses %s', (_case, root, fake, reason, tag) => {
    const { calls, result } = publish(root(), fake, tag);

    expect(result).toThrow(reason);
    expect(creates(calls)).toHaveLength(0);
  });
});

describe('RELEASE-06-11 draft only', () => {
  const forbidden = (calls: { command: string; args: string[] }[]) =>
    calls.filter(
      ({ command, args }) =>
        (command === 'git' && ['push', 'tag'].some((verb) => args.includes(verb))) ||
        (command === 'gh' && args[0] === 'release' && !['view', 'create'].includes(args[1])) ||
        args.some((arg) => arg.startsWith('--draft=')),
    );

  it('creates exactly one draft with the four files for a final tag', () => {
    const { calls, result, lines } = publish(makeRoot());

    expect(result()).toEqual({
      tag: 'v0.26.0',
      prerelease: false,
      url: 'https://github.com/owner/repo/releases/tag/untagged-1',
    });
    const created = creates(calls);

    expect(created).toHaveLength(1);
    const { args } = created[0];

    expect(args.slice(0, 5)).toEqual(['release', 'create', 'v0.26.0', '--draft', '--verify-tag']);
    expect(args).not.toContain('--prerelease');
    expect(args.slice(-4)).toEqual([dmg, zip, 'SHA256SUMS', 'release.json']);
    expect(args.slice(args.indexOf('--notes-file') + 2)).toEqual([
      dmg,
      zip,
      'SHA256SUMS',
      'release.json',
    ]);
    expect(args.join(' ')).not.toContain('marcosmoura/stache');
    expect(forbidden(calls)).toEqual([]);
    expect(lines.join('\n')).toMatch(/untagged-1/);
    expect(lines.join('\n')).toMatch(/promote it by hand/);
  });

  it('adds --prerelease only for a candidate tag, and accepts a peeled annotated tag', () => {
    const { calls, result } = publish(makeRoot(), {
      tags: 'v0.26.0-rc.1\n',
      remote: `${'f'.repeat(40)}\trefs/tags/v0.26.0-rc.1\n${head}\trefs/tags/v0.26.0-rc.1^{}\n`,
    });

    expect(result().prerelease).toBe(true);
    const created = creates(calls);

    expect(created).toHaveLength(1);
    expect(created[0].args).toContain('--prerelease');
    expect(created[0].args).toContain('--draft');
    expect(forbidden(calls)).toEqual([]);
  });

  it('uses the tag named with --tag among several', () => {
    const { calls, result } = publish(makeRoot(), { tags: 'v0.26.0-rc.1\nv0.26.0\n' }, 'v0.26.0');

    expect(result().tag).toBe('v0.26.0');
    expect(creates(calls)[0].args[2]).toBe('v0.26.0');
  });

  it('asks gh for the Quality push runs of the exact commit', () => {
    const { calls, result } = publish(makeRoot());

    result();
    const list = calls.find((call) => call.command === 'gh' && call.args[0] === 'run');

    expect(list?.args).toEqual([
      'run',
      'list',
      '--workflow',
      'quality.yml',
      '--commit',
      head,
      '--event',
      'push',
      '--json',
      'databaseId,headSha,status,conclusion,createdAt,event',
    ]);
  });

  it('removes the generated notes file', () => {
    const { calls, result } = publish(makeRoot());

    result();
    const args = creates(calls)[0].args;

    expect(existsSync(args[args.indexOf('--notes-file') + 1])).toBe(false);
  });
});

describe('RELEASE-06-12 notes', () => {
  const notes = releaseNotes({ tag: 'v0.26.0', dmg, zip, certificateSha256: fingerprint });

  it('holds the requirements, first-open steps, limitations and fingerprint', () => {
    expect(notes).toContain('Apple silicon (arm64)');
    expect(notes).toContain('macOS 26 or later');
    expect(notes).toContain('System Settings → Privacy & Security');
    expect(notes).toContain('**Open Anyway**');
    expect(notes).toContain(
      'https://github.com/marcosmoura/stache/blob/v0.26.0/docs/guides/install.md',
    );

    for (const limitation of limitations) {
      expect(notes).toContain(`- ${limitation}`);
    }

    expect(notes).toContain(fingerprint);
    expect(notes).toMatch(/Every release should show the same fingerprint/);
    expect(notes).not.toMatch(/\bis notarized|verified by Apple|\bsafe\b/i);
  });

  it('is what publish uploads', () => {
    const fake = publish(makeRoot());

    fake.result();
    expect(fake.notes()).toBe(notes);
  });
});

const read = (path: string) => readFileSync(join(import.meta.dirname, '../..', path), 'utf8');
const flat = (text: string) => text.replaceAll(/\s+/g, ' ');

describe('RELEASE-06-13 published limitations', () => {
  it.each(['README.md', 'docs/guides/install.md'])('%s states every limitation', (path) => {
    const doc = flat(read(path));

    for (const limitation of limitations) {
      expect(doc).toContain(limitation);
    }

    expect(doc).toMatch(/No release has been published yet/);
  });
});

describe('RELEASE-06-14 operational checklist', () => {
  it('lists the checklist in order in the release guide', () => {
    const guide = read('docs/guides/release.md');
    const checklist = guide.split('### Operational checklist\n')[1]?.split('\n## ')[0] ?? '';
    const steps = flat(checklist)
      .split(/ (?=\d\. )/)
      .filter((step) => /^\d\. /.test(step));

    expect(steps.map((step) => step.slice(0, 2))).toEqual(['1.', '2.', '3.', '4.', '5.', '6.']);

    for (const [index, phrase] of [
      'tag `v<version>` and push the tag',
      'Wait for Quality to pass on that exact commit',
      'bun run release --self-sign',
      '`bun run release:publish`, which creates the draft',
      'Download the DMG from the draft in a browser',
      'Promote the draft by hand',
    ].entries()) {
      expect(steps[index]).toContain(phrase);
    }

    expect(guide).toMatch(/No release has been published yet/);
    expect(guide).toContain('bun run release:publish');
  });
});
