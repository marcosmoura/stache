import { createHash } from 'node:crypto';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

import { afterEach, describe, expect, it } from 'vitest';

import { qualityGate, tagVersionProblem, type WorkflowRun } from './release/gate';
import {
  artifactNames,
  type BundleInspection,
  listFlattenedResources,
  readReleaseConfig,
  readWorkspaceVersion,
  ReleaseError,
  type ReleaseMode,
  type RunOptions,
  runRelease,
  scrubbedAppleVariables,
  scrubbedReleaseVariables,
  verifyBundle,
  type VerifierExpectations,
} from './release/pipeline';

const machO = Buffer.from('cffaedfe00000000', 'hex');

const releaseConf = {
  productName: 'Stache',
  version: '0.26.0',
  identifier: 'com.marcosmoura.stache',
};

const goodPlist = {
  LSUIElement: true,
  NSHighResolutionCapable: true,
  LSApplicationCategoryType: 'public.app-category.utilities',
  LSMinimumSystemVersion: '26.0',
  CFBundleShortVersionString: '0.26.0',
  CFBundleIdentifier: 'com.marcosmoura.stache',
  CFBundleName: 'Stache',
};

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

function makeRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'release-test-'));

  roots.push(root);
  const media = join(root, 'src/crates/app/resources/media-control');
  const framework = join(media, 'Adapter.framework');

  mkdirSync(join(framework, 'Versions/A'), { recursive: true });
  writeFileSync(join(framework, 'Versions/A/Adapter'), machO);
  symlinkSync('A', join(framework, 'Versions/Current'));
  symlinkSync('Versions/Current/Adapter', join(framework, 'Adapter'));
  writeFileSync(join(media, 'adapter.pl'), '#!/usr/bin/perl\n');
  writeFileSync(join(root, 'src/crates/app/tauri.conf.json'), JSON.stringify(releaseConf));
  writeFileSync(join(root, 'src/crates/app/Entitlements.plist'), '<plist/>');
  writeFileSync(
    join(root, 'Cargo.toml'),
    '[workspace]\n\n[workspace.package]\nversion = "0.26.0"\n',
  );

  return root;
}

interface FakeOptions {
  fail?: (command: string, args: string[]) => boolean;
  plist?: Record<string, unknown>;
  noApplicationsLink?: boolean;
  codesignFailsUntilResigned?: boolean;
  dirty?: boolean;
  /** `security find-identity -p codesigning` output. */
  identities?: string;
  authority?: string;
  /** Authority reported for the DMG only. */
  dmgAuthority?: string;
  requirement?: string;
}

const identity = 'Stache Test Signer';
const fingerprint = 'AB'.repeat(32);

function fakeRunner(root: string, options: FakeOptions = {}) {
  const calls: { command: string; args: string[]; options: RunOptions }[] = [];
  let resigned = false;
  const ok = (stdout = '') => ({ status: 0, stdout, stderr: '' });

  const run = (command: string, args: string[], runOptions: RunOptions = {}) => {
    calls.push({ command, args, options: runOptions });

    if (options.fail?.(command, args)) {
      return { status: 1, stdout: '', stderr: `injected ${command} failure` };
    }

    switch (command) {
      case 'git':
        if (args.includes('rev-parse')) {
          return ok('abc123\n');
        }

        return ok(options.dirty ? ' M Cargo.toml\n' : '');
      case 'bun': {
        const app = join(root, 'target/release/bundle/macos/Stache.app/Contents');

        mkdirSync(join(app, 'MacOS'), { recursive: true });
        writeFileSync(join(app, 'MacOS/stache'), machO);
        writeFileSync(join(app, 'Info.plist'), '<plist/>');
        mkdirSync(join(app, 'Resources'), { recursive: true });
        writeFileSync(join(app, 'Resources/icon.icns'), 'icns');
        const source = join(root, 'src/crates/app/resources/media-control');

        for (const file of listFlattenedResources(source)) {
          mkdirSync(dirname(join(app, 'Resources/media-control', file)), { recursive: true });
          writeFileSync(
            join(app, 'Resources/media-control', file),
            readFileSync(join(source, file)),
          );
        }

        return ok();
      }
      case 'lipo':
        return ok('arm64\n');
      case 'otool':
        return ok(`${args[1]}:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n`);
      case 'plutil':
        return ok(JSON.stringify(options.plist ?? goodPlist));
      case 'codesign':
        if (args[0] === '--force') {
          resigned = true;

          return ok();
        }

        if (args[0] === '-dvvv') {
          const authority =
            (args.at(-1)!.endsWith('.dmg') ? options.dmgAuthority : undefined) ??
            options.authority ??
            identity;

          return {
            status: 0,
            stdout: '',
            stderr: `CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1\nAuthority=${authority}\n`,
          };
        }

        if (args[0] === '-d' && args[1] === '-r-') {
          return ok(
            options.requirement ??
              `designated => identifier "com.marcosmoura.stache" and certificate root = H"${'ab'.repeat(20)}"\n`,
          );
        }

        return options.codesignFailsUntilResigned && !resigned
          ? { status: 1, stdout: '', stderr: 'nested code is not signed' }
          : ok();
      case 'ditto':
        if (args[0] === '-c') {
          writeFileSync(args.at(-1) as string, 'zip');
        } else if (args[0] === '-x') {
          mkdirSync(join(args[3], 'Stache.app/Contents'), { recursive: true });
        } else {
          cpSync(args[0], args[1], { recursive: true });
        }

        return ok();
      case 'hdiutil':
        if (args[0] === 'create') {
          writeFileSync(args.at(-1) as string, 'dmg');
        } else if (args[0] === 'attach') {
          const mount = args[args.indexOf('-mountpoint') + 1];

          mkdirSync(join(mount, 'Stache.app/Contents'), { recursive: true });
          writeFileSync(join(mount, 'Stache.app/Contents/Info.plist'), '');

          if (!options.noApplicationsLink) {
            symlinkSync('/Applications', join(mount, 'Applications'));
          }
        }

        return ok();
      case 'shasum': {
        const cwd = runOptions.cwd as string;
        const lines = args.slice(2).map(
          (name) =>
            `${createHash('sha256')
              .update(readFileSync(join(cwd, name)))
              .digest('hex')}  ${name}`,
        );

        return ok(`${lines.join('\n')}\n`);
      }
      case 'security':
        if (args[0] === 'find-identity') {
          return ok(
            options.identities ??
              `  1) ${'C'.repeat(40)} "${identity}" (CSSMERR_TP_NOT_TRUSTED)\n     1 identities found\n`,
          );
        }

        return ok(`SHA-256 hash: ${fingerprint}\nSHA-1 hash: ${'D'.repeat(40)}\n`);
      default:
        throw new Error(`unexpected command ${command}`);
    }
  };

  return { run, calls };
}

function release(
  root: string,
  options: FakeOptions = {},
  env: NodeJS.ProcessEnv = {},
  mode?: ReleaseMode,
) {
  const fake = fakeRunner(root, options);
  const lines: string[] = [];
  const result = () =>
    runRelease({ root, run: fake.run, env, mode, log: (line) => lines.push(line) });

  return { fake, lines, result };
}

const output = (root: string) => join(root, 'target/release-artifacts');

describe('RELEASE-02-01 names', () => {
  it('derives names from either config shape without a hardcoded product', () => {
    const final = readReleaseConfig(JSON.stringify(releaseConf));
    const next = readReleaseConfig(
      JSON.stringify({
        ...releaseConf,
        productName: 'Stache Next',
        identifier: 'com.marcosmoura.stache.next',
      }),
    );

    expect(artifactNames(next.productName, next.configVersion)).toEqual({
      app: 'Stache Next.app',
      dmg: 'Stache_Next_0.26.0_aarch64.dmg',
      zip: 'Stache_Next_0.26.0_aarch64.zip',
      volume: 'Stache Next',
    });
    expect(artifactNames(final.productName, final.configVersion)).toMatchObject({
      app: 'Stache.app',
      dmg: 'Stache_0.26.0_aarch64.dmg',
      zip: 'Stache_0.26.0_aarch64.zip',
    });
  });

  it('reads the authoritative workspace version', () => {
    expect(
      readWorkspaceVersion('[package]\nversion = "9"\n[workspace.package]\nversion = "0.26.0"\n'),
    ).toBe('0.26.0');
    expect(() => readWorkspaceVersion('[package]\nversion = "9"\n')).toThrow(ReleaseError);
  });
});

describe('RELEASE-02-02 fail closed', () => {
  const steps: [string, (command: string, args: string[]) => boolean][] = [
    ['build', (command) => command === 'bun'],
    ['verifier', (command) => command === 'lipo'],
    ['hdiutil', (command, args) => command === 'hdiutil' && args[0] === 'create'],
    ['ditto', (command, args) => command === 'ditto' && args[0] === '-c'],
    ['checksum', (command) => command === 'shasum'],
  ];

  it.each(steps)('a %s failure throws and leaves no SHA256SUMS', (_step, fail) => {
    const root = makeRoot();

    release(root).result();
    expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(true);

    expect(release(root, { fail }).result).toThrow(ReleaseError);
    expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(false);
    expect(readdirSync(join(root, 'target')).filter((name) => name.includes('staging'))).toEqual(
      [],
    );
  });

  it('detaches the DMG even when the mounted check fails', () => {
    const root = makeRoot();
    const { fake, result } = release(root, { noApplicationsLink: true });

    expect(result).toThrow(/Applications link/);
    const hdiutil = fake.calls
      .filter((call) => call.command === 'hdiutil')
      .map((call) => call.args[0]);

    expect(hdiutil.slice(hdiutil.indexOf('attach'))).toEqual(['attach', 'detach']);
  });

  it('stops with no SHA256SUMS when the verifier itself refuses', () => {
    const root = makeRoot();
    const { fake, result } = release(root, { plist: { ...goodPlist, LSUIElement: false } });

    expect(result).toThrow(/bundle verifier refused.*\n.*LSUIElement/);
    expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(false);
    expect(fake.calls.some((call) => call.command === 'hdiutil')).toBe(false);
  });

  it('sweeps stale staging directories, detaching a leftover mount first', () => {
    const root = makeRoot();
    const stale = join(root, 'target/release-artifacts.staging-1/.work/mount-abc');

    mkdirSync(stale, { recursive: true });
    const { fake, result } = release(root);

    result();
    expect(fake.calls[0]).toMatchObject({ command: 'hdiutil', args: ['detach', '-force', stale] });
    expect(existsSync(join(root, 'target/release-artifacts.staging-1'))).toBe(false);
  });

  it('never fails the run when the stale sweep cannot detach', () => {
    const root = makeRoot();

    mkdirSync(join(root, 'target/release-artifacts.staging-2/.work/mount-x'), { recursive: true });
    const { result } = release(root, {
      fail: (command, args) => command === 'hdiutil' && args.includes('-force'),
    });

    expect(result().version).toBe('0.26.0');
    expect(existsSync(join(root, 'target/release-artifacts.staging-2'))).toBe(false);
  });
});

describe('RELEASE-02-03 bundle verifier', () => {
  const expected: VerifierExpectations = {
    executable: 'stache',
    version: '0.26.0',
    identifier: 'com.marcosmoura.stache',
    productName: 'Stache',
    mediaControl: ['adapter.pl', 'Adapter.framework/Adapter'],
  };
  const nested = 'Contents/Resources/media-control/Adapter.framework/Adapter';
  const good: BundleInspection = {
    files: [
      'Contents/Info.plist',
      'Contents/MacOS/stache',
      'Contents/Resources/icon.icns',
      'Contents/Resources/media-control/adapter.pl',
      'Contents/_CodeSignature/CodeResources',
      nested,
    ],
    macosEntries: ['stache'],
    mainArchs: 'arm64\n',
    mainLinks:
      'stache:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n\t/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit (x)\n',
    nestedArchs: { [nested]: 'arm64\n' },
    infoPlist: goodPlist,
  };

  it('accepts the expected bundle', () => {
    expect(verifyBundle(good, expected)).toEqual([]);
  });

  const refusals: [string, Partial<BundleInspection>][] = [
    ['a non-arm64 executable', { mainArchs: 'x86_64 arm64\n' }],
    ['an extra file in Contents/MacOS', { macosEntries: ['generate-schema', 'stache'] }],
    [
      'an unexpected media-control file',
      { files: [...good.files, 'Contents/Resources/media-control/extra'] },
    ],
    ['a non-arm64 nested Mach-O', { nestedArchs: { [nested]: 'x86_64\n' } }],
    ['a non-system library', { mainLinks: 'stache:\n\t@rpath/libfoo.dylib (x)\n' }],
    ['a version mismatch', { infoPlist: { ...goodPlist, CFBundleShortVersionString: '0.25.0' } }],
    ['a missing LSUIElement', { infoPlist: { ...goodPlist, LSUIElement: undefined } }],
    [
      'a different CFBundleIdentifier',
      { infoPlist: { ...goodPlist, CFBundleIdentifier: 'com.marcosmoura.stache.next' } },
    ],
    ['a different CFBundleName', { infoPlist: { ...goodPlist, CFBundleName: 'Stache Next' } }],
    [
      'a framework outside media-control',
      { files: [...good.files, 'Contents/Frameworks/X.framework/X'] },
    ],
    ['an extra file in Contents/Resources', { files: [...good.files, 'Contents/Resources/x.js'] }],
    ['a file in an unexpected directory', { files: [...good.files, 'Contents/Helpers/x'] }],
    [
      'a missing icon.icns',
      { files: good.files.filter((file) => file !== 'Contents/Resources/icon.icns') },
    ],
  ];

  it.each(refusals)('refuses %s', (_case, change) => {
    expect(verifyBundle({ ...good, ...change }, expected)).not.toEqual([]);
  });

  it('derives the media-control set from the source as Tauri flattens it', () => {
    const root = makeRoot();

    // A file link becomes a plain file; a directory link (Versions/Current) is dropped.
    expect(listFlattenedResources(join(root, 'src/crates/app/resources/media-control'))).toEqual([
      'Adapter.framework/Adapter',
      'Adapter.framework/Versions/A/Adapter',
      'adapter.pl',
    ]);

    const { fake, result } = release(root);

    result();
    const lipoTargets = fake.calls
      .filter((call) => call.command === 'lipo')
      .map((call) => call.args[1]);

    expect(lipoTargets.filter((path) => path.includes('media-control'))).toHaveLength(2);
  });
});

describe('RELEASE-02-04 replacement', () => {
  it('replaces the output directory on a second run', () => {
    const root = makeRoot();

    release(root).result();
    writeFileSync(join(output(root), 'stale.dmg'), 'old');
    release(root).result();

    expect(readdirSync(output(root)).toSorted()).toEqual([
      'SHA256SUMS',
      'Stache_0.26.0_aarch64.dmg',
      'Stache_0.26.0_aarch64.zip',
      'release.json',
    ]);
    const sums = readFileSync(join(output(root), 'SHA256SUMS'), 'utf8')
      .trim()
      .split('\n');

    expect(sums).toHaveLength(2);
    expect(sums.map((line) => line.split('  ')[1])).toEqual([
      'Stache_0.26.0_aarch64.dmg',
      'Stache_0.26.0_aarch64.zip',
    ]);
  });
});

describe('RELEASE-02-05 environment', () => {
  it('removes every APPLE_*, signing and token variable from the build child', () => {
    const root = makeRoot();
    const env = Object.fromEntries([
      ...scrubbedAppleVariables.map((name) => [name, 'secret']),
      ...scrubbedReleaseVariables.map((name) => [name, 'secret']),
      ['PATH', '/usr/bin'],
    ]);
    const { fake, result } = release(root, {}, env);

    result();
    const build = fake.calls.find((call) => call.command === 'bun');

    expect(build?.args).toEqual(['run', 'tauri:build']);
    expect(build?.options.env).toEqual({ PATH: '/usr/bin' });
    expect(env.APPLE_ID).toBe('secret');
    expect(scrubbedReleaseVariables).toEqual(
      expect.arrayContaining(['STACHE_SIGNING_IDENTITY', 'GH_TOKEN', 'GITHUB_TOKEN']),
    );
  });
});

describe('RELEASE-02-07 honest modes', () => {
  it('records ad-hoc, not notarized, and never claims otherwise', () => {
    const root = makeRoot();
    const { lines, result } = release(root);

    result();
    const record = JSON.parse(readFileSync(join(output(root), 'release.json'), 'utf8'));

    expect(record).toEqual({
      version: '0.26.0',
      commit: 'abc123',
      treeClean: true,
      signing: 'ad-hoc',
      notarized: false,
      adHocResigned: false,
      artifacts: ['Stache_0.26.0_aarch64.dmg', 'Stache_0.26.0_aarch64.zip'],
    });

    for (const line of lines) {
      expect(line).not.toMatch(/developer id|stapled/i);

      if (/notariz/i.test(line)) {
        expect(line).toMatch(/not notarized|notarized: false/);
      }
    }

    expect(lines.some((line) => line.includes('ad-hoc'))).toBe(true);
  });

  it('re-signs inside out without --deep when strict verification fails, and records it', () => {
    const root = makeRoot();
    const { fake, result } = release(root, { codesignFailsUntilResigned: true });

    expect(result().adHocResigned).toBe(true);
    const signs = fake.calls.filter(
      (call) => call.command === 'codesign' && call.args[0] === '--force',
    );

    expect(signs.every((call) => !call.args.includes('--deep'))).toBe(true);
    const targets = signs.map((call) => call.args.at(-1) as string);

    expect(targets.at(-1)).toMatch(/Stache\.app$/);
    expect(targets.at(-2)).toMatch(/MacOS\/stache$/);
    expect(signs.at(-1)?.args).toContain('--entitlements');
    expect(signs.slice(0, -2).every((call) => !call.args.includes('--entitlements'))).toBe(true);
  });
});

describe('RELEASE-02-06 workflow trigger', () => {
  it('runs quality on v*-rc.* tags beside main pushes and pull requests', () => {
    // Narrow textual check: no YAML parser is a declared dependency.
    const workflow = readFileSync(
      join(import.meta.dirname, '../.github/workflows/quality.yml'),
      'utf8',
    );
    const on = /^on:\n((?: {2}.*\n|\n)*)/m.exec(workflow)?.[1] ?? '';

    expect(on).toMatch(/^ {2}push:\n {4}branches: \[main\]\n {4}tags: \['v\*-rc\.\*'\]\n/);
    expect(on).toMatch(/^ {2}pull_request:/m);
    expect(workflow).toMatch(/run: bun run tauri:build\n\s+- run: bun run release\n/);
    expect(workflow).toMatch(/shasum -a 256 -c SHA256SUMS/);
    expect(workflow).not.toMatch(/upload-artifact|secrets\./);
  });
});

const signEnv: NodeJS.ProcessEnv = { STACHE_SIGNING_IDENTITY: identity };

const readRecord = (root: string) =>
  JSON.parse(readFileSync(join(output(root), 'release.json'), 'utf8'));

const signCalls = (calls: { command: string; args: string[] }[]) =>
  calls.filter((call) => call.command === 'codesign' && call.args[0] === '--force');

describe('RELEASE-03-11 --self-sign refusals', () => {
  const refusals: [string, NodeJS.ProcessEnv, FakeOptions?][] = [
    ['a missing identity', {}],
    ['an empty identity', { STACHE_SIGNING_IDENTITY: '' }],
    ['a blank identity', { STACHE_SIGNING_IDENTITY: '   ' }],
    ['the ad-hoc identity "-"', { STACHE_SIGNING_IDENTITY: '-' }],
    ['an APPLE_* name instead of the script input', { APPLE_SIGNING_IDENTITY: identity }],
    [
      'a name absent from the keychain listing',
      signEnv,
      { identities: '  1) AAAA "Another Signer"\n     1 identities found\n' },
    ],
    [
      'a listed name that only contains the identity',
      signEnv,
      { identities: `  1) AAAA "${identity} 2"\n     1 identities found\n` },
    ],
    ['an empty keychain listing', signEnv, { identities: '     0 identities found\n' }],
    [
      'a failing keychain listing',
      signEnv,
      { fail: (command, args) => command === 'security' && args[0] === 'find-identity' },
    ],
  ];

  it.each(refusals)('refuses %s, runs no build and never falls back', (_case, env, options) => {
    const root = makeRoot();
    const { fake, result } = release(root, options, env, 'self-sign');

    expect(result).toThrow(ReleaseError);
    expect(fake.calls.every((call) => call.command === 'security')).toBe(true);
    expect(fake.calls.some((call) => call.command === 'bun')).toBe(false);
    expect(signCalls(fake.calls)).toEqual([]);
    expect(existsSync(output(root))).toBe(false);
  });

  it('asks the keychain for code-signing identities through the runner', () => {
    const root = makeRoot();
    const { fake, result } = release(root, {}, signEnv, 'self-sign');

    result();
    expect(fake.calls[0]).toMatchObject({
      command: 'security',
      args: ['find-identity', '-p', 'codesigning'],
    });
  });

  it('never hardcodes an identity name in the script', () => {
    for (const file of ['release/pipeline.ts', 'release/publish.ts']) {
      const source = readFileSync(join(import.meta.dirname, file), 'utf8');

      expect(source).not.toMatch(/Stache App|Developer ID/);
    }
  });
});

/** A short label per external step, in call order. */
function steps(calls: { command: string; args: string[] }[]): string[] {
  return calls.flatMap(({ command, args }) => {
    const target = (args.at(-1) ?? '')
      .split('/')
      .slice(-2)
      .join('/')
      .replace(/^release-artifacts\.staging-\d+\//, '');

    if (command === 'codesign') {
      const kind =
        { '--force': 'sign', '--verify': 'verify', '-dvvv': 'details', '-d': 'requirement' }[
          args[0]
        ] ?? args[0];

      return [`${kind} ${target}`];
    }

    if (command === 'ditto' && args[0] === '-c') {
      return [`zip ${target}`];
    }

    if (command === 'hdiutil' && args[0] === 'create') {
      return ['dmg create'];
    }

    return command === 'shasum' ? ['shasum'] : [];
  });
}

describe('RELEASE-03-12 signing arguments and order', () => {
  it('signs inside out with runtime and no timestamp, never --deep, then the DMG', () => {
    const root = makeRoot();
    const { fake, result } = release(root, {}, signEnv, 'self-sign');

    result();
    const signs = signCalls(fake.calls);

    expect(signs).toHaveLength(5);

    for (const call of signs) {
      expect(call.args).not.toContain('--deep');
      expect(call.args).toContain('--timestamp=none');
      expect(call.args).not.toContain('--timestamp');
      expect(call.args.join(' ')).toContain(`--sign ${identity}`);
    }

    expect(
      signs.slice(0, 4).every((call) => call.args.join(' ').includes('--options runtime')),
    ).toBe(true);
    expect(signs.slice(0, 2).every((call) => !call.args.includes('--entitlements'))).toBe(true);
    expect(signs.slice(2, 4).every((call) => call.args.includes('--entitlements'))).toBe(true);
    expect(signs[2].args[signs[2].args.indexOf('--entitlements') + 1]).toBe(
      join(root, 'src/crates/app/Entitlements.plist'),
    );
    expect(signs[4].args).toEqual([
      '--force',
      '--timestamp=none',
      '--sign',
      identity,
      expect.stringMatching(/Stache_0\.26\.0_aarch64\.dmg$/),
    ]);

    const verifies = fake.calls.filter(
      (call) => call.command === 'codesign' && call.args[0] === '--verify',
    );

    expect(verifies.every((call) => call.args.includes('--strict'))).toBe(true);
    expect(
      verifies
        .filter((call) => !call.args.at(-1)!.endsWith('.dmg'))
        .every((call) => call.args.includes('--deep')),
    ).toBe(true);
    expect(verifies.find((call) => call.args.at(-1)!.endsWith('.dmg'))?.args).toEqual([
      '--verify',
      '--strict',
      expect.stringMatching(/Stache_0\.26\.0_aarch64\.dmg$/),
    ]);

    expect(steps(fake.calls)).toEqual([
      'sign Adapter.framework/Adapter',
      'sign A/Adapter',
      'sign MacOS/stache',
      'sign dmg-root/Stache.app',
      'verify dmg-root/Stache.app',
      'details dmg-root/Stache.app',
      'requirement dmg-root/Stache.app',
      'dmg create',
      'sign Stache_0.26.0_aarch64.dmg',
      'verify Stache_0.26.0_aarch64.dmg',
      'details Stache_0.26.0_aarch64.dmg',
      'zip Stache_0.26.0_aarch64.zip',
      expect.stringMatching(/^verify mount-.*\/Stache\.app$/),
      expect.stringMatching(/^details mount-.*\/Stache\.app$/),
      expect.stringMatching(/^requirement mount-.*\/Stache\.app$/),
      expect.stringMatching(/^verify unzip-.*\/Stache\.app$/),
      expect.stringMatching(/^details unzip-.*\/Stache\.app$/),
      expect.stringMatching(/^requirement unzip-.*\/Stache\.app$/),
      'shasum',
    ]);

    for (const call of fake.calls) {
      expect(['xcrun', 'spctl', 'notarytool', 'stapler']).not.toContain(call.command);
      expect(call.args.join(' ')).not.toMatch(/notarytool|stapler|spctl/);
    }
  });

  it('shares the inside-out helper with the ad-hoc re-sign fallback', () => {
    const root = makeRoot();
    const { fake, result } = release(root, { codesignFailsUntilResigned: true });

    result();
    expect(signCalls(fake.calls).map((call) => call.args.slice(0, 4))).toEqual(
      Array.from({ length: 4 }, () => ['--force', '--timestamp=none', '--sign', '-']),
    );
  });
});

describe('RELEASE-03-13 fail closed while signing', () => {
  const failures: [string, FakeOptions][] = [
    [
      'nested sign',
      {
        fail: (c, a) =>
          c === 'codesign' && a[0] === '--force' && a.at(-1)!.includes('media-control'),
      },
    ],
    [
      'main executable sign',
      {
        fail: (c, a) =>
          c === 'codesign' && a[0] === '--force' && a.at(-1)!.endsWith('MacOS/stache'),
      },
    ],
    [
      'bundle sign',
      { fail: (c, a) => c === 'codesign' && a[0] === '--force' && a.at(-1)!.endsWith('.app') },
    ],
    [
      'DMG sign',
      { fail: (c, a) => c === 'codesign' && a[0] === '--force' && a.at(-1)!.endsWith('.dmg') },
    ],
    [
      'DMG strict verify',
      { fail: (c, a) => c === 'codesign' && a[0] === '--verify' && a.at(-1)!.endsWith('.dmg') },
    ],
    [
      'DMG signature details',
      { fail: (c, a) => c === 'codesign' && a[0] === '-dvvv' && a.at(-1)!.endsWith('.dmg') },
    ],
    ['strict verify', { fail: (c, a) => c === 'codesign' && a[0] === '--verify' }],
    [
      'strict verify in the DMG',
      { fail: (c, a) => c === 'codesign' && a[0] === '--verify' && a.at(-1)!.includes('/mount-') },
    ],
    [
      'strict verify in the zip',
      { fail: (c, a) => c === 'codesign' && a[0] === '--verify' && a.at(-1)!.includes('/unzip-') },
    ],
    ['signature details', { fail: (c, a) => c === 'codesign' && a[0] === '-dvvv' }],
    ['requirement display', { fail: (c, a) => c === 'codesign' && a[0] === '-d' }],
    ['certificate lookup', { fail: (c, a) => c === 'security' && a[0] === 'find-certificate' }],
    ['an authority that is not the identity', { authority: 'Someone Else' }],
    ['a DMG authority that is not the identity', { dmgAuthority: 'Someone Else' }],
    [
      'a cdhash-based designated requirement',
      { requirement: `designated => cdhash H"${'ab'.repeat(20)}"\n` },
    ],
    ['a requirement without a certificate', { requirement: 'designated => identifier "x"\n' }],
  ];

  it.each(failures)('a %s failure stops the run without SHA256SUMS', (_step, options) => {
    const root = makeRoot();

    release(root).result();
    const { fake, result } = release(root, options, signEnv, 'self-sign');

    expect(result).toThrow(ReleaseError);
    expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(false);
    expect(existsSync(join(output(root), 'release.json'))).toBe(false);
    expect(fake.calls.some((call) => call.command === 'shasum')).toBe(false);
    expect(readdirSync(join(root, 'target')).filter((name) => name.includes('staging'))).toEqual(
      [],
    );
  });

  it('accepts a requirement anchored on the certificate as leaf or as root', () => {
    for (const anchor of ['leaf', 'root']) {
      const root = makeRoot();
      const requirement = `designated => identifier "x" and certificate ${anchor} = H"${'ab'.repeat(20)}"\n`;

      release(root, { requirement }, signEnv, 'self-sign').result();

      expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(true);
    }
  });

  it('refuses a missing hardened-runtime flag', () => {
    const root = makeRoot();
    const fake = fakeRunner(root);
    const run = (command: string, args: string[], options?: RunOptions) =>
      command === 'codesign' && args[0] === '-dvvv'
        ? { status: 0, stdout: '', stderr: `flags=0x2(adhoc)\nAuthority=${identity}\n` }
        : fake.run(command, args, options);

    expect(() => runRelease({ root, run, env: signEnv, mode: 'self-sign', log: () => {} })).toThrow(
      /hardened-runtime/,
    );
    expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(false);
  });
});

describe('RELEASE-03-14 self-signed record and environment', () => {
  it('records self-signed, not notarized and the certificate fingerprint', () => {
    const root = makeRoot();
    const { fake, lines, result } = release(root, {}, signEnv, 'self-sign');

    result();
    expect(readRecord(root)).toEqual({
      version: '0.26.0',
      commit: 'abc123',
      treeClean: true,
      signing: 'self-signed',
      notarized: false,
      adHocResigned: false,
      certificateSha256: fingerprint,
      artifacts: ['Stache_0.26.0_aarch64.dmg', 'Stache_0.26.0_aarch64.zip'],
    });
    expect(fake.calls[1]).toMatchObject({
      command: 'security',
      args: ['find-certificate', '-a', '-c', identity, '-Z'],
    });

    for (const line of lines) {
      expect(line).not.toMatch(/developer id|stapl/i);

      if (/notariz/i.test(line)) {
        expect(line).toMatch(/not notarized|notarized: false/);
      }
    }
  });

  it('refuses an ambiguous certificate name', () => {
    const root = makeRoot();
    const fake = fakeRunner(root);
    const run = (command: string, args: string[], options?: RunOptions) =>
      command === 'security' && args[0] === 'find-certificate'
        ? {
            status: 0,
            stdout: `SHA-256 hash: ${'A'.repeat(64)}\nSHA-256 hash: ${'B'.repeat(64)}\n`,
            stderr: '',
          }
        : fake.run(command, args, options);

    expect(() => runRelease({ root, run, env: signEnv, mode: 'self-sign', log: () => {} })).toThrow(
      /found 2/,
    );
    expect(fake.calls.some((call) => call.command === 'bun')).toBe(false);
  });

  it('keeps the signing input and tokens out of the build child', () => {
    const root = makeRoot();
    const env = { ...signEnv, GH_TOKEN: 't', GITHUB_TOKEN: 't', APPLE_ID: 'a', PATH: '/x' };
    const { fake, result } = release(root, {}, env, 'self-sign');

    result();
    expect(fake.calls.find((call) => call.command === 'bun')?.options.env).toEqual({ PATH: '/x' });
  });
});

describe('RELEASE-03-15 paid path removed', () => {
  const read = (path: string) => readFileSync(join(import.meta.dirname, '..', path), 'utf8');

  it('has no release workflow and no paid signing mode', () => {
    expect(existsSync(join(import.meta.dirname, '../.github/workflows/release.yml'))).toBe(false);

    for (const file of ['scripts/release.ts', 'scripts/release/pipeline.ts', 'package.json']) {
      const source = read(file);

      expect(source).not.toMatch(
        /['"]--sign['"]\s*:|run release --sign|--dry-run-signing|notarytool|stapler|spctl/,
      );
      expect(source).not.toMatch(/STACHE_TEAM_ID|STACHE_NOTARY_|release-evidence/);
    }
  });

  it('keeps the ad-hoc step, its checksum check and the candidate trigger, with no secret', () => {
    const workflow = read('.github/workflows/quality.yml');

    expect(workflow).toMatch(
      /- run: bun run release\n\s+- name: [^\n]*\n\s+working-directory: target\/release-artifacts\n\s+run: shasum -a 256 -c SHA256SUMS\n$/,
    );
    expect(workflow).toContain("tags: ['v*-rc.*']");
    expect(workflow).not.toMatch(/secrets\.|--self-sign|--dry-run-signing|release --sign/);
  });
});

describe('release gate (kept for Part 06)', () => {
  it('matches the tag against the authoritative version', () => {
    expect(tagVersionProblem('v0.26.0', '0.26.0')).toBeNull();
    expect(tagVersionProblem('v0.26.0-rc.2', '0.26.0')).toBeNull();
    expect(tagVersionProblem('v0.26.1', '0.26.0')).toMatch(/does not match/);
    expect(tagVersionProblem('v0.26.0-beta', '0.26.0')).toMatch(/not v<version>/);
    expect(tagVersionProblem('0.26.0', '0.26.0')).toMatch(/not v<version>/);
  });

  const sha = 'a'.repeat(40);
  const run = (change: Partial<WorkflowRun>): WorkflowRun => ({
    headSha: sha,
    event: 'push',
    status: 'completed',
    conclusion: 'success',
    createdAt: '2026-11-13T10:00:00Z',
    ...change,
  });

  it.each<[string, WorkflowRun[], string]>([
    ['passes a successful run for the SHA', [run({ databaseId: 7 })], 'pass'],
    ['blocks a missing run', [], 'blocked'],
    ['blocks a run for another SHA only', [run({ headSha: 'b'.repeat(40) })], 'blocked'],
    ['waits on a pending run', [run({ status: 'in_progress', conclusion: null })], 'pending'],
    ['blocks a failed run', [run({ conclusion: 'failure' })], 'blocked'],
    ['ignores a pull_request run', [run({ event: 'pull_request' })], 'blocked'],
    [
      'ignores a newer pull_request run beside a push run',
      [
        run({}),
        run({ event: 'pull_request', createdAt: '2026-11-13T11:00:00Z', conclusion: 'failure' }),
      ],
      'pass',
    ],
    ['blocks a cancelled run', [run({ conclusion: 'cancelled' })], 'blocked'],
    [
      'decides on the newest run for the SHA',
      [run({}), run({ createdAt: '2026-11-13T11:00:00Z', conclusion: 'failure' })],
      'blocked',
    ],
  ])('%s', (_case, runs, state) => {
    expect(qualityGate(runs, sha).state).toBe(state);
  });
});

describe('RELEASE-05-11 no cask', () => {
  it('has no cask module and writes exactly the four outputs', () => {
    expect(existsSync(join(import.meta.dirname, 'release/cask.ts'))).toBe(false);
    expect(readFileSync(join(import.meta.dirname, 'release/pipeline.ts'), 'utf8')).not.toMatch(
      /cask/i,
    );

    const root = makeRoot();

    release(root).result();

    expect(readdirSync(output(root)).toSorted()).toEqual([
      'SHA256SUMS',
      'Stache_0.26.0_aarch64.dmg',
      'Stache_0.26.0_aarch64.zip',
      'release.json',
    ]);
    const sums = readFileSync(join(output(root), 'SHA256SUMS'), 'utf8')
      .trim()
      .split('\n');
    const record = JSON.parse(readFileSync(join(output(root), 'release.json'), 'utf8'));

    expect(sums.map((line) => line.split('  ')[1])).toEqual([
      'Stache_0.26.0_aarch64.dmg',
      'Stache_0.26.0_aarch64.zip',
    ]);
    expect(record.artifacts).toEqual(['Stache_0.26.0_aarch64.dmg', 'Stache_0.26.0_aarch64.zip']);
  });
});

describe('RELEASE-05-06 install guide', () => {
  const read = (path: string) => readFileSync(join(import.meta.dirname, '..', path), 'utf8');
  const guide = read('docs/guides/install.md');
  const section = (name: string) => guide.split(`## ${name}\n`)[1]?.split('\n## ')[0] ?? '';

  it('has the sections, in order, after the Read when line', () => {
    expect(guide.split('\n')[2]).toMatch(/^Read when /);
    const headings = guide.split('\n').filter((line) => line.startsWith('## '));

    expect(headings).toEqual([
      '## Requirements',
      '## Limitations',
      '## Install',
      '## First open',
      '## Upgrade',
      '## Coexistence',
      '## Legacy removal',
      '## Config migration',
      '## CLI access',
      '## Uninstall',
    ]);
    expect(guide).toMatch(/no release has been published yet/i);
  });

  it('explains the missing notarization and gives the Open Anyway route in First open', () => {
    const firstOpen = section('First open');

    expect(firstOpen).toMatch(/not notarized/);
    expect(firstOpen).toContain('Privacy & Security');
    expect(firstOpen).toContain('**Open Anyway**');
    expect(firstOpen).toMatch(/once for each version/);
  });

  it('says there is no Homebrew route', () => {
    expect(guide).toMatch(/There is no Homebrew cask or formula/);
  });

  it('never recommends a kill, quarantine removal, Gatekeeper bypass or brew install', () => {
    // Split so `app/tests/identity.rs`'s process-name tool scan does not match this list itself.
    for (const forbidden of [
      ['kill', 'all'].join(''),
      ['p', 'kill'].join(''),
      ['p', 'grep stache'].join(''),
      'xattr -d',
      'xattr -cr',
      'spctl --master-disable',
      'brew install',
    ]) {
      expect(guide).not.toContain(forbidden);
    }

    // `--force` appears only on `stache config migrate`.
    for (const line of guide.split('\n').filter((line) => line.includes('--force'))) {
      expect(line).toMatch(/stache config migrate/);
    }
  });

  it('is linked from README.md and docs/index.md', () => {
    expect(read('README.md')).toContain('docs/guides/install.md');
    expect(read('docs/index.md')).toContain('guides/install.md');
  });
});
