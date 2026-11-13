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

import {
  artifactNames,
  type BundleInspection,
  listFlattenedResources,
  readReleaseConfig,
  readWorkspaceVersion,
  ReleaseError,
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
}

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
        return ok(args.includes('rev-parse') ? 'abc123\n' : '');
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
      default:
        throw new Error(`unexpected command ${command}`);
    }
  };

  return { run, calls };
}

function release(root: string, options: FakeOptions = {}, env: NodeJS.ProcessEnv = {}) {
  const fake = fakeRunner(root, options);
  const lines: string[] = [];
  const result = () => runRelease({ root, run: fake.run, env, log: (line) => lines.push(line) });

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
