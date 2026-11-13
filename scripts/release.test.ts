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
  verifyBundle,
  type VerifierExpectations,
} from './release/pipeline';

const machO = Buffer.from('cffaedfe00000000', 'hex');

const nextConf = {
  productName: 'Stache Next',
  version: '0.26.0',
  identifier: 'com.marcosmoura.stache.next',
};

const goodPlist = {
  LSUIElement: true,
  NSHighResolutionCapable: true,
  LSApplicationCategoryType: 'public.app-category.utilities',
  LSMinimumSystemVersion: '26.0',
  CFBundleShortVersionString: '0.26.0',
  CFBundleIdentifier: 'com.marcosmoura.stache.next',
  CFBundleName: 'Stache Next',
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
  writeFileSync(join(root, 'src/crates/app/tauri.conf.json'), JSON.stringify(nextConf));
  writeFileSync(join(root, 'src/crates/app/Entitlements.plist'), '<plist/>');
  writeFileSync(
    join(root, 'Cargo.toml'),
    '[workspace]\n\n[workspace.package]\nversion = "0.26.0"\n',
  );

  return root;
}

interface FakeOptions {
  fail?: (command: string, args: string[]) => boolean;
  codesignFailsUntilResigned?: boolean;
  notaryStatus?: string;
  dirty?: boolean;
}

const identity = 'Developer ID Application: Marcos Moura (ABCDE12345)';

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
        const app = join(root, 'target/release/bundle/macos/Stache Next.app/Contents');

        mkdirSync(join(app, 'MacOS'), { recursive: true });
        writeFileSync(join(app, 'MacOS/stache'), machO);
        writeFileSync(join(app, 'Info.plist'), '<plist/>');
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
        return ok(JSON.stringify(goodPlist));
      case 'codesign':
        if (args[0] === '--force') {
          resigned = true;

          return ok();
        }

        if (args[0] === '-dvvv') {
          return {
            status: 0,
            stdout: '',
            stderr: `CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1\nAuthority=${identity}\nTeamIdentifier=ABCDE12345\nTimestamp=Nov 13, 2026\n`,
          };
        }

        return options.codesignFailsUntilResigned && !resigned
          ? { status: 1, stdout: '', stderr: 'nested code is not signed' }
          : ok();
      case 'ditto':
        if (args[0] === '-c') {
          writeFileSync(args.at(-1) as string, 'zip');
        } else if (args[0] === '-x') {
          mkdirSync(join(args[3], 'Stache Next.app/Contents'), { recursive: true });
        } else {
          cpSync(args[0], args[1], { recursive: true });
        }

        return ok();
      case 'hdiutil':
        if (args[0] === 'create') {
          writeFileSync(args.at(-1) as string, 'dmg');
        } else if (args[0] === 'attach') {
          const mount = args[args.indexOf('-mountpoint') + 1];

          mkdirSync(join(mount, 'Stache Next.app/Contents'), { recursive: true });
          writeFileSync(join(mount, 'Stache Next.app/Contents/Info.plist'), '');
          symlinkSync('/Applications', join(mount, 'Applications'));
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
      case 'xcrun':
        if (args[1] === 'submit') {
          return ok(JSON.stringify({ id: 'sub-1', status: options.notaryStatus ?? 'Accepted' }));
        }

        if (args[1] === 'log') {
          writeFileSync(args.at(-1) as string, '{"issues":[]}');
        }

        return ok();
      case 'spctl':
        return ok();
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
    const next = readReleaseConfig(JSON.stringify(nextConf));
    const final = readReleaseConfig(
      JSON.stringify({ ...nextConf, productName: 'Stache', identifier: 'com.marcosmoura.stache' }),
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
    const { fake, result } = release(root, {
      fail: (command, args) => command === 'ditto' && args[0] === '-x',
    });

    expect(result).toThrow(ReleaseError);
    expect(fake.calls.some((call) => call.command === 'hdiutil' && call.args[0] === 'detach')).toBe(
      true,
    );
  });
});

describe('RELEASE-02-03 bundle verifier', () => {
  const expected: VerifierExpectations = {
    executable: 'stache',
    version: '0.26.0',
    identifier: 'com.marcosmoura.stache.next',
    productName: 'Stache Next',
    mediaControl: ['adapter.pl', 'Adapter.framework/Adapter'],
  };
  const nested = 'Contents/Resources/media-control/Adapter.framework/Adapter';
  const good: BundleInspection = {
    files: [
      'Contents/Info.plist',
      'Contents/MacOS/stache',
      'Contents/Resources/media-control/adapter.pl',
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
      { infoPlist: { ...goodPlist, CFBundleIdentifier: 'com.marcosmoura.stache' } },
    ],
    ['a different CFBundleName', { infoPlist: { ...goodPlist, CFBundleName: 'Stache' } }],
    [
      'a framework outside media-control',
      { files: [...good.files, 'Contents/Frameworks/X.framework/X'] },
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
      'Stache_Next_0.26.0_aarch64.dmg',
      'Stache_Next_0.26.0_aarch64.zip',
      'release.json',
    ]);
    const sums = readFileSync(join(output(root), 'SHA256SUMS'), 'utf8')
      .trim()
      .split('\n');

    expect(sums).toHaveLength(2);
    expect(sums.map((line) => line.split('  ')[1])).toEqual([
      'Stache_Next_0.26.0_aarch64.dmg',
      'Stache_Next_0.26.0_aarch64.zip',
    ]);
  });
});

describe('RELEASE-02-05 environment', () => {
  it('removes every APPLE_* credential variable from the build child', () => {
    const root = makeRoot();
    const env = Object.fromEntries([
      ...scrubbedAppleVariables.map((name) => [name, 'secret']),
      ['PATH', '/usr/bin'],
    ]);
    const { fake, result } = release(root, {}, env);

    result();
    const build = fake.calls.find((call) => call.command === 'bun');

    expect(build?.args).toEqual(['run', 'tauri:build']);
    expect(build?.options.env).toEqual({ PATH: '/usr/bin' });
    expect(env.APPLE_ID).toBe('secret');
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
      artifacts: ['Stache_Next_0.26.0_aarch64.dmg', 'Stache_Next_0.26.0_aarch64.zip'],
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

    expect(targets.at(-1)).toMatch(/Stache Next\.app$/);
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

const signEnv: NodeJS.ProcessEnv = {
  STACHE_SIGNING_IDENTITY: identity,
  STACHE_TEAM_ID: 'ABCDE12345',
  STACHE_NOTARY_KEY_ID: 'KEY123',
  STACHE_NOTARY_ISSUER: 'issuer-uuid',
  STACHE_NOTARY_KEY_PATH: '/tmp/key.p8',
};

const readRecord = (root: string) =>
  JSON.parse(readFileSync(join(output(root), 'release.json'), 'utf8'));

describe('RELEASE-03-02 --sign refusals', () => {
  const refusals: [string, NodeJS.ProcessEnv][] = [
    ['no input at all', {}],
    ['a missing identity', { ...signEnv, STACHE_SIGNING_IDENTITY: undefined }],
    ['a missing team', { ...signEnv, STACHE_TEAM_ID: undefined }],
    [
      'no notary credential',
      {
        ...signEnv,
        STACHE_NOTARY_KEY_ID: '',
        STACHE_NOTARY_ISSUER: '',
        STACHE_NOTARY_KEY_PATH: '',
      },
    ],
    ['a partial API key', { ...signEnv, STACHE_NOTARY_ISSUER: undefined }],
    ['two notary credentials', { ...signEnv, STACHE_NOTARY_KEYCHAIN_PROFILE: 'stache' }],
    ['the Stache App identity', { ...signEnv, STACHE_SIGNING_IDENTITY: 'Stache App' }],
    [
      'an Apple Development identity',
      { ...signEnv, STACHE_SIGNING_IDENTITY: 'Apple Development: Marcos Moura (ABCDE12345)' },
    ],
    ['the ad-hoc identity', { ...signEnv, STACHE_SIGNING_IDENTITY: '-' }],
    [
      "another team's Developer ID",
      { ...signEnv, STACHE_SIGNING_IDENTITY: 'Developer ID Application: X (ZZZZZ99999)' },
    ],
    [
      'APPLE_* names instead of the script inputs',
      { APPLE_SIGNING_IDENTITY: identity, APPLE_TEAM_ID: 'ABCDE12345' },
    ],
  ];

  it.each(refusals)('refuses %s before building, with no fallback', (_case, env) => {
    const root = makeRoot();
    const { fake, result } = release(root, {}, env, 'sign');

    expect(result).toThrow(ReleaseError);
    expect(fake.calls).toEqual([]);
    expect(existsSync(output(root))).toBe(false);
  });

  it('refuses a dirty tree before building', () => {
    const root = makeRoot();
    const { fake, result } = release(root, { dirty: true }, signEnv, 'sign');

    expect(result).toThrow(/clean git tree/);
    expect(fake.calls.map((call) => call.command)).toEqual(['git', 'git']);
  });

  it('accepts a keychain profile instead of the API key', () => {
    const root = makeRoot();
    const env = {
      STACHE_SIGNING_IDENTITY: identity,
      STACHE_TEAM_ID: 'ABCDE12345',
      STACHE_NOTARY_KEYCHAIN_PROFILE: 'stache-notary',
    };
    const { fake, result } = release(root, {}, env, 'sign');

    result();
    const submit = fake.calls.find((call) => call.args[1] === 'submit');

    expect(submit?.args.slice(-2)).toEqual(['--keychain-profile', 'stache-notary']);
  });
});

/** A short label per external step, in call order. */
function steps(calls: { command: string; args: string[] }[]): string[] {
  return calls.flatMap(({ command, args }) => {
    const path = (command === 'xcrun' && args[1] === 'submit' ? args[2] : args.at(-1)) ?? '';
    const target = path
      .split('/')
      .slice(-2)
      .join('/')
      .replace(/^release-artifacts\.staging-\d+\//, '');

    if (command === 'codesign' && args[0] === '--force') {
      return [`sign ${target}`];
    }

    if (command === 'codesign') {
      return [`${args[0] === '-dvvv' ? 'details' : 'verify'} ${target}`];
    }

    if (command === 'xcrun') {
      return [`${args[0]} ${args[1]} ${args[1] === 'log' ? '' : target}`.trim()];
    }

    if (command === 'spctl') {
      return [`spctl ${args[2]} ${target}`];
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

describe('RELEASE-03-03 signing arguments and order', () => {
  it('signs with runtime and timestamp, never --deep, in the stated order', () => {
    const root = makeRoot();
    const { fake, result } = release(root, {}, signEnv, 'sign');

    const record = result();
    const signs = fake.calls.filter(
      (call) => call.command === 'codesign' && call.args[0] === '--force',
    );

    for (const call of signs) {
      expect(call.args).not.toContain('--deep');
      expect(call.args).toContain('--timestamp');
      expect(call.args).toContain(identity);
    }

    // App signatures carry the hardened runtime; entitlements only on executable and bundle.
    expect(
      signs.slice(0, 4).every((call) => call.args.join(' ').includes('--options runtime')),
    ).toBe(true);
    expect(signs.slice(0, 2).every((call) => !call.args.includes('--entitlements'))).toBe(true);
    expect(signs.slice(2, 4).every((call) => call.args.includes('--entitlements'))).toBe(true);

    const verifies = fake.calls.filter(
      (call) => call.command === 'codesign' && call.args[0] === '--verify',
    );

    expect(
      verifies.every((call) => call.args.includes('--strict') && call.args.includes('--deep')),
    ).toBe(true);

    expect(steps(fake.calls)).toEqual([
      'sign Adapter.framework/Adapter',
      'sign A/Adapter',
      'sign MacOS/stache',
      'sign dmg-root/Stache Next.app',
      'verify dmg-root/Stache Next.app',
      'details dmg-root/Stache Next.app',
      'zip .work/submit-Stache_Next_0.26.0_aarch64.zip',
      'notarytool submit .work/submit-Stache_Next_0.26.0_aarch64.zip',
      'stapler staple dmg-root/Stache Next.app',
      'stapler validate dmg-root/Stache Next.app',
      'dmg create',
      'sign Stache_Next_0.26.0_aarch64.dmg',
      'notarytool submit Stache_Next_0.26.0_aarch64.dmg',
      'stapler staple Stache_Next_0.26.0_aarch64.dmg',
      'stapler validate Stache_Next_0.26.0_aarch64.dmg',
      'zip Stache_Next_0.26.0_aarch64.zip',
      'verify dmg-root/Stache Next.app',
      'details dmg-root/Stache Next.app',
      'spctl execute dmg-root/Stache Next.app',
      'spctl open Stache_Next_0.26.0_aarch64.dmg',
      'stapler validate Stache_Next_0.26.0_aarch64.dmg',
      expect.stringMatching(/^details mount-.*\/Stache Next\.app$/),
      expect.stringMatching(/^stapler validate mount-.*\/Stache Next\.app$/),
      expect.stringMatching(/^verify unzip-.*\/Stache Next\.app$/),
      expect.stringMatching(/^details unzip-.*\/Stache Next\.app$/),
      expect.stringMatching(/^stapler validate unzip-.*\/Stache Next\.app$/),
      'shasum',
    ]);
    expect(record).toMatchObject({
      signing: 'developer-id',
      notarized: true,
      mode: 'sign',
      notarySubmissions: { app: 'sub-1', dmg: 'sub-1' },
    });
    expect(JSON.stringify(readRecord(root))).not.toMatch(/KEY123|issuer-uuid|key\.p8/);
  });

  it('dry run uses identity "-", --timestamp=none and skips Apple steps honestly', () => {
    const root = makeRoot();
    const { fake, lines, result } = release(root, {}, signEnv, 'dry-run-signing');

    result();
    const signs = fake.calls.filter(
      (call) => call.command === 'codesign' && call.args[0] === '--force',
    );

    expect(signs).toHaveLength(5);
    expect(
      signs.every((call) => call.args.includes('-') && call.args.includes('--timestamp=none')),
    ).toBe(true);
    expect(
      signs.slice(0, 4).every((call) => call.args.join(' ').includes('--options runtime')),
    ).toBe(true);
    expect(signs.every((call) => !call.args.includes('--deep'))).toBe(true);
    expect(fake.calls.some((call) => ['xcrun', 'spctl'].includes(call.command))).toBe(false);
    expect(fake.calls.filter((call) => call.args[0] === '-dvvv')).toHaveLength(4);
    expect(readRecord(root)).toMatchObject({
      signing: 'ad-hoc',
      notarized: false,
      mode: 'dry-run-signing',
    });
    expect(lines.filter((line) => /skipped/.test(line)).length).toBeGreaterThanOrEqual(5);
    expect(lines.some((line) => /Accepted|passed.*(spctl|stapler)/.test(line))).toBe(false);
  });
});

describe('RELEASE-03-04 fail closed while signing', () => {
  const failures: [string, (command: string, args: string[]) => boolean][] = [
    [
      'nested sign',
      (c, a) => c === 'codesign' && a[0] === '--force' && a.at(-1)!.includes('media-control'),
    ],
    [
      'main executable sign',
      (c, a) => c === 'codesign' && a[0] === '--force' && a.at(-1)!.endsWith('MacOS/stache'),
    ],
    ['bundle sign', (c, a) => c === 'codesign' && a[0] === '--force' && a.at(-1)!.endsWith('.app')],
    ['DMG sign', (c, a) => c === 'codesign' && a[0] === '--force' && a.at(-1)!.endsWith('.dmg')],
    ['strict verify', (c, a) => c === 'codesign' && a[0] === '--verify'],
    ['signature details', (c, a) => c === 'codesign' && a[0] === '-dvvv'],
    ['app notarization', (c, a) => c === 'xcrun' && a[1] === 'submit' && a[2].endsWith('.zip')],
    ['DMG notarization', (c, a) => c === 'xcrun' && a[1] === 'submit' && a[2].endsWith('.dmg')],
    ['staple', (c, a) => c === 'xcrun' && a[1] === 'staple'],
    ['staple validation', (c, a) => c === 'xcrun' && a[1] === 'validate'],
    ['spctl', (c) => c === 'spctl'],
  ];

  it.each(failures)(
    'a %s failure stops the run without SHA256SUMS or notarized: true',
    (_step, fail) => {
      const root = makeRoot();
      const { fake, result } = release(root, { fail }, signEnv, 'sign');

      expect(result).toThrow(ReleaseError);
      expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(false);
      expect(existsSync(join(output(root), 'release.json'))).toBe(false);
      expect(fake.calls.some((call) => call.command === 'shasum')).toBe(false);
      // The injected step was reached.
      expect(fake.calls.some((call) => fail(call.command, call.args))).toBe(true);
    },
  );

  it.each(failures.filter(([step]) => !/notariz|staple|spctl/.test(step)))(
    'a dry-run %s failure also stops the run',
    (_step, fail) => {
      const root = makeRoot();

      expect(release(root, { fail }, {}, 'dry-run-signing').result).toThrow(ReleaseError);
      expect(existsSync(join(output(root), 'SHA256SUMS'))).toBe(false);
    },
  );

  it('fetches the notarytool log on rejection and keeps it as evidence', () => {
    const root = makeRoot();
    const { fake, result } = release(root, { notaryStatus: 'Invalid' }, signEnv, 'sign');

    expect(result).toThrow(/Invalid.*sub-1.*log saved/);
    const log = fake.calls.find((call) => call.args[1] === 'log');

    expect(log?.args.slice(0, 3)).toEqual(['notarytool', 'log', 'sub-1']);
    expect(readdirSync(join(root, 'target/release-evidence'))).toEqual(['notary-app-sub-1.json']);
    expect(existsSync(output(root))).toBe(false);
    expect(fake.calls.some((call) => call.args[1] === 'staple')).toBe(false);
  });
});

describe('RELEASE-03-06 release workflow', () => {
  // Narrow textual checks: no YAML parser is a declared dependency.
  const workflow = readFileSync(
    join(import.meta.dirname, '../.github/workflows/release.yml'),
    'utf8',
  );

  it('triggers only on v* tags', () => {
    const on = /^on:\n((?: {2}.*\n|\n)*)/m.exec(workflow)?.[1] ?? '';

    expect(on.trim()).toBe("push:\n    tags: ['v*']");
    expect(workflow).not.toMatch(/pull_request|workflow_dispatch|branches:/);
    expect(workflow).toMatch(/runs-on: macos-26/);
  });

  it('gates secrets behind the release environment and the signing step', () => {
    expect(workflow).toMatch(/^ {4}environment: release$/m);
    const stepsText = workflow.split(/\n {6}- /);
    const withSecrets = stepsText.filter((step) => step.includes('secrets.'));

    expect(withSecrets).toHaveLength(1);
    expect(withSecrets[0]).toMatch(/bun run release --sign/);
    const cleanup = stepsText.find((step) => step.includes('delete-keychain'));

    expect(cleanup).toMatch(/if: always\(\)/);
    expect(cleanup).toMatch(/rm -f .*stache-notary-key\.p8/);
  });

  it('checks the tag and Quality for the exact SHA before signing, and creates drafts only', () => {
    const gate = workflow.indexOf('bun scripts/release/gate.ts "$TAG" "$GITHUB_SHA"');
    const sign = workflow.indexOf('bun run release --sign');

    expect(gate).toBeGreaterThan(0);
    expect(gate).toBeLessThan(sign);
    expect(workflow).toMatch(/--workflow quality\.yml --commit "\$GITHUB_SHA"/);
    const creates = workflow.match(/gh release create[^\n]*/g) ?? [];

    expect(creates).toHaveLength(1);
    expect(creates[0]).toMatch(/--draft/);
    expect(workflow).not.toMatch(/gh release edit|--draft=false/);
    expect(workflow).toMatch(/if: failure\(\)[\s\S]*gh release delete/);
  });

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

describe('RELEASE-03-05 quality runs the signing dry run', () => {
  it('runs --dry-run-signing and verifies its checksums', () => {
    const workflow = readFileSync(
      join(import.meta.dirname, '../.github/workflows/quality.yml'),
      'utf8',
    );

    expect(workflow).toMatch(
      /run: bun run release --dry-run-signing\n\s+- name: [^\n]*\n\s+working-directory: target\/release-artifacts\n\s+run: shasum -a 256 -c SHA256SUMS/,
    );
  });
});
