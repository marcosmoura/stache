/**
 * `bun run release:publish` (`RELEASE-06-10` to `RELEASE-06-12`).
 *
 * Publishes what `bun run release --self-sign` already built as a draft GitHub release. It never
 * builds, signs, tags, pushes or promotes: every precondition refuses before the single
 * `gh release create --draft`, and promotion stays the owner's manual step. The repository is the
 * one `gh` resolves from the checkout's `origin`.
 */
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

import { qualityGate, tagVersionProblem, type WorkflowRun } from './gate';
import {
  artifactNames,
  readReleaseConfig,
  readWorkspaceVersion,
  ReleaseError,
  type RunResult,
  type Runner,
} from './pipeline';

/** The published limitations (Part 06), word for word. The notes and the doc checks use this. */
export const limitations = [
  'Apple silicon (arm64) only; no Intel or universal build. macOS 26 or later.',
  'Not notarized by Apple. The app is self-signed, so macOS blocks the first open until you allow it in System Settings.',
  'No Homebrew cask or formula.',
  'No auto-updater: upgrades are manual.',
  'No launch-at-login item.',
  'No automatic configuration migration: run `stache config migrate`.',
  'A development build (`Stache Next`) is separate from the release and can run beside it.',
  'An old Stache from before 0.26 must be removed first; the new app refuses to start beside it, with no visible dialog when opened from Finder.',
  'Tray Quit, Cmd-Q and SIGTERM run the coordinated teardown. Logout, restart and shutdown are not verified to.',
  "The app's own path is not a command line; link `stache` as the install guide describes.",
] as const;

export interface NotesFacts {
  tag: string;
  dmg: string;
  zip: string;
  certificateSha256: string;
}

/** The release notes, built from the same facts every time. */
export function releaseNotes({ tag, dmg, zip, certificateSha256 }: NotesFacts): string {
  const guide = `https://github.com/marcosmoura/stache/blob/${tag}/docs/guides/install.md`;

  return `## Requirements

- A Mac with Apple silicon (arm64).
- macOS 26 or later.

## Install

Download \`${dmg}\`, open it and drag \`Stache.app\` onto \`Applications\`. The same app is also in
\`${zip}\`. Check the downloads with \`shasum -a 256 -c SHA256SUMS\` in the download folder.

## First open

Stache is not notarized by Apple. It is signed with the author's own certificate, so macOS blocks
the first open of a downloaded copy:

1. Open \`Stache.app\` from \`Applications\` and dismiss the message.
2. Open System Settings → Privacy & Security.
3. Scroll to the message about Stache and choose **Open Anyway**, then confirm.

This is needed once for each version you download. The full guide, including upgrade, command-line
access and uninstall, is at ${guide}.

## Limitations

${limitations.map((line) => `- ${line}`).join('\n')}

## Signing certificate

SHA-256 fingerprint of the signing certificate:

\`${certificateSha256}\`

Every release should show the same fingerprint. If it differs, the release was not signed with the
same certificate.
`;
}

export interface PublishDeps {
  root: string;
  run: Runner;
  log: (line: string) => void;
  /** The tag to publish when several matching tags point at `HEAD`. */
  tag?: string;
}

export interface PublishResult {
  tag: string;
  prerelease: boolean;
  url: string;
}

const git = ['-c', 'core.fsmonitor=false'];

function check(result: RunResult, what: string): RunResult {
  if (result.status !== 0) {
    throw new ReleaseError(`${what} failed: ${result.stderr.trim() || result.stdout.trim()}`);
  }

  return result;
}

interface ReleaseRecordFile {
  commit?: unknown;
  treeClean?: unknown;
  signing?: unknown;
  certificateSha256?: unknown;
}

export function runPublish({ root, run, log, tag: requested }: PublishDeps): PublishResult {
  const config = readReleaseConfig(
    readFileSync(join(root, 'src/crates/app/tauri.conf.json'), 'utf8'),
  );
  const version = readWorkspaceVersion(readFileSync(join(root, 'Cargo.toml'), 'utf8'));
  const names = artifactNames(config.productName, version);
  const output = join(root, 'target/release-artifacts');
  const recordPath = join(output, 'release.json');

  // The built artifacts: a self-signed record from a clean tree at the current HEAD.
  if (!existsSync(recordPath)) {
    throw new ReleaseError(`${recordPath} is missing; run \`bun run release --self-sign\` first`);
  }

  const record = JSON.parse(readFileSync(recordPath, 'utf8')) as ReleaseRecordFile;

  if (record.signing !== 'self-signed') {
    throw new ReleaseError(
      `release.json records signing "${String(record.signing)}", not "self-signed"; rebuild with --self-sign`,
    );
  }

  if (typeof record.certificateSha256 !== 'string' || record.certificateSha256 === '') {
    throw new ReleaseError('release.json has no certificateSha256');
  }

  if (record.treeClean !== true) {
    throw new ReleaseError('release.json records a dirty working tree; rebuild from a clean tree');
  }

  const head = check(
    run('git', [...git, 'rev-parse', 'HEAD'], { cwd: root }),
    'git rev-parse',
  ).stdout.trim();

  if (record.commit !== head) {
    throw new ReleaseError(
      `release.json records commit ${String(record.commit)}, but HEAD is ${head}; rebuild at HEAD`,
    );
  }

  const status = check(
    run('git', [...git, 'status', '--porcelain'], { cwd: root }),
    'git status',
  ).stdout.trim();

  if (status !== '') {
    throw new ReleaseError('the working tree is not clean; commit or remove the changes first');
  }

  const files = [names.dmg, names.zip, 'SHA256SUMS', 'release.json'];
  const missing = files.filter((file) => !existsSync(join(output, file)));

  if (missing.length > 0) {
    throw new ReleaseError(`missing artifacts in ${output}: ${missing.join(', ')}`);
  }

  if (run('shasum', ['-a', '256', '-c', 'SHA256SUMS'], { cwd: output }).status !== 0) {
    throw new ReleaseError('shasum -a 256 -c SHA256SUMS failed; the artifacts do not match');
  }

  // The tag on HEAD, matching the authoritative version.
  const tags = check(
    run(
      'git',
      [...git, 'for-each-ref', '--points-at', 'HEAD', '--format=%(refname:short)', 'refs/tags'],
      { cwd: root },
    ),
    'git for-each-ref',
  )
    .stdout.split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '' && tagVersionProblem(line, version) === null);
  let tag: string;

  if (requested !== undefined) {
    const problem = tagVersionProblem(requested, version);

    if (problem) {
      throw new ReleaseError(problem);
    }

    if (!tags.includes(requested)) {
      throw new ReleaseError(`tag ${requested} does not point at HEAD (${head})`);
    }

    tag = requested;
  } else if (tags.length === 0) {
    throw new ReleaseError(
      `HEAD (${head}) carries no tag v${version} or v${version}-rc.<n>; tag it first`,
    );
  } else if (tags.length > 1) {
    throw new ReleaseError(
      `HEAD carries several matching tags (${tags.join(', ')}); name one with --tag <tag>`,
    );
  } else {
    tag = tags[0];
  }

  // The same tag on origin, at the same commit (an annotated tag's peeled line wins).
  const remote = check(
    run('git', [...git, 'ls-remote', '--tags', 'origin', `refs/tags/${tag}`], { cwd: root }),
    'git ls-remote',
  )
    .stdout.split('\n')
    .map((line) => line.trim().split(/\s+/))
    .filter((parts) => parts.length === 2);
  const peeled = remote.find(([, ref]) => ref === `refs/tags/${tag}^{}`);
  const direct = remote.find(([, ref]) => ref === `refs/tags/${tag}`);
  const remoteSha = (peeled ?? direct)?.[0];

  if (!remoteSha) {
    throw new ReleaseError(`tag ${tag} is not on origin; push it first`);
  }

  if (remoteSha !== head) {
    throw new ReleaseError(`tag ${tag} on origin points at ${remoteSha}, not HEAD (${head})`);
  }

  // A successful Quality push run for that exact commit.
  const runs = check(
    run(
      'gh',
      [
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
      ],
      { cwd: root },
    ),
    'gh run list',
  );
  const gate = qualityGate(JSON.parse(runs.stdout || '[]') as WorkflowRun[], head);

  if (gate.state === 'pending') {
    throw new ReleaseError(`${gate.reason}; wait for it to finish and run this again`);
  }

  if (gate.state === 'blocked') {
    throw new ReleaseError(`${gate.reason}; Quality must pass on a push of this commit`);
  }

  // No release for the tag yet.
  const view = run('gh', ['release', 'view', tag, '--json', 'url'], { cwd: root });

  if (view.status === 0) {
    throw new ReleaseError(`a release for ${tag} already exists`);
  }

  if (!/release not found/i.test(view.stderr)) {
    throw new ReleaseError(`gh release view ${tag} failed: ${view.stderr.trim()}`);
  }

  // The one action: a draft, never published.
  const prerelease = tag.includes('-rc.');
  const notesDir = mkdtempSync(join(tmpdir(), 'stache-release-notes-'));
  const notes = join(notesDir, 'notes.md');

  try {
    writeFileSync(
      notes,
      releaseNotes({
        tag,
        dmg: names.dmg,
        zip: names.zip,
        certificateSha256: record.certificateSha256,
      }),
    );
    const created = check(
      run(
        'gh',
        [
          'release',
          'create',
          tag,
          '--draft',
          '--verify-tag',
          ...(prerelease ? ['--prerelease'] : []),
          '--title',
          tag,
          '--notes-file',
          notes,
          ...files,
        ],
        { cwd: output },
      ),
      'gh release create',
    );
    const url = created.stdout.trim().split('\n').at(-1) ?? '';

    log(`Created draft release ${tag}${prerelease ? ' (prerelease)' : ''}: ${url}`);
    log('It is a draft. Download and check it, then promote it by hand on GitHub.');

    return { tag, prerelease, url };
  } finally {
    rmSync(notesDir, { recursive: true, force: true });
  }
}

if (import.meta.main) {
  const run: Runner = (command, args, options = {}) => {
    const result = spawnSync(command, args, {
      cwd: options.cwd,
      env: options.env ?? process.env,
      encoding: 'utf8',
      stdio: 'pipe',
    });

    if (result.error) {
      return { status: 1, stdout: '', stderr: result.error.message };
    }

    return { status: result.status ?? 1, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
  };

  try {
    const args = process.argv.slice(2);
    const tagged = args.length === 2 && args[0] === '--tag';

    if (args.length !== 0 && !tagged) {
      throw new ReleaseError(
        `usage: bun run release:publish [--tag <tag>], got: ${args.join(' ')}`,
      );
    }

    runPublish({
      root: resolve(import.meta.dirname, '../..'),
      run,
      log: (line) => console.log(line),
      tag: tagged ? args[1] : undefined,
    });
  } catch (error) {
    console.error(
      `publish refused: ${error instanceof ReleaseError ? error.message : String(error)}`,
    );
    process.exit(1);
  }
}
