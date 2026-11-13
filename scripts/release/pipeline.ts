import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readSync,
  closeSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { join, relative } from 'node:path';

/** Variables that could drive credentialed signing or notarization in the build child. */
export const scrubbedAppleVariables = [
  'APPLE_SIGNING_IDENTITY',
  'APPLE_CERTIFICATE',
  'APPLE_CERTIFICATE_PASSWORD',
  'APPLE_ID',
  'APPLE_PASSWORD',
  'APPLE_TEAM_ID',
  'APPLE_API_ISSUER',
  'APPLE_API_KEY',
  'APPLE_API_KEY_PATH',
] as const;

export interface RunOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  /** Stream output to the terminal instead of capturing it. */
  inherit?: boolean;
}

export interface RunResult {
  status: number;
  stdout: string;
  stderr: string;
}

export type Runner = (command: string, args: string[], options?: RunOptions) => RunResult;

export class ReleaseError extends Error {}

export interface ReleaseConfig {
  productName: string;
  identifier: string;
  configVersion: string;
}

export interface ArtifactNames {
  app: string;
  dmg: string;
  zip: string;
  volume: string;
}

export function readReleaseConfig(tauriConfJson: string): ReleaseConfig {
  const conf = JSON.parse(tauriConfJson) as Record<string, unknown>;
  const { productName, identifier, version } = conf;

  if (typeof productName !== 'string' || typeof identifier !== 'string') {
    throw new ReleaseError('tauri.conf.json lacks productName or identifier');
  }

  if (typeof version !== 'string') {
    throw new ReleaseError('tauri.conf.json lacks version');
  }

  return { productName, identifier, configVersion: version };
}

/** Reads `[workspace.package] version` from the root `Cargo.toml`. */
export function readWorkspaceVersion(cargoToml: string): string {
  const section = /^\[workspace\.package\]\s*$([\s\S]*?)(?=^\[|$(?![\s\S]))/m.exec(cargoToml);
  const version = section && /^version\s*=\s*"([^"]+)"/m.exec(section[1]);

  if (!version) {
    throw new ReleaseError('Cargo.toml lacks [workspace.package] version');
  }

  return version[1];
}

export function artifactNames(productName: string, version: string): ArtifactNames {
  const base = `${productName.replaceAll(' ', '_')}_${version}_aarch64`;

  return { app: `${productName}.app`, dmg: `${base}.dmg`, zip: `${base}.zip`, volume: productName };
}

export function scrubbedEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const copy = { ...env };

  for (const name of scrubbedAppleVariables) {
    delete copy[name];
  }

  return copy;
}

/**
 * Files under a directory, relative and sorted, flattened as Tauri's resource copy does:
 * a link to a file becomes a plain file and a link to a directory is dropped.
 */
export function listFlattenedResources(dir: string): string[] {
  const out: string[] = [];
  const walk = (current: string) => {
    for (const name of readdirSync(current)) {
      const path = join(current, name);
      const isLink = lstatSync(path).isSymbolicLink();

      if (statSync(path).isDirectory()) {
        if (!isLink) {
          walk(path);
        }
      } else {
        out.push(relative(dir, path));
      }
    }
  };

  walk(dir);

  return out.toSorted();
}

/** Every file under a directory without following links (the bundle as built). */
function listEntries(dir: string): string[] {
  const out: string[] = [];
  const walk = (current: string) => {
    for (const name of readdirSync(current)) {
      const path = join(current, name);

      if (lstatSync(path).isDirectory()) {
        walk(path);
      } else {
        out.push(relative(dir, path));
      }
    }
  };

  walk(dir);

  return out.toSorted();
}

const machOMagics = new Set([
  'feedfacf',
  'cffaedfe',
  'cafebabe',
  'bebafeca',
  'feedface',
  'cefaedfe',
]);

function isMachO(path: string): boolean {
  if (!lstatSync(path).isFile()) {
    return false;
  }

  const fd = openSync(path, 'r');
  const buffer = Buffer.alloc(4);

  try {
    return readSync(fd, buffer, 0, 4, 0) === 4 && machOMagics.has(buffer.toString('hex'));
  } finally {
    closeSync(fd);
  }
}

export interface BundleInspection {
  /** Every file in the bundle, relative to the `.app` directory. */
  files: string[];
  /** Entries directly under `Contents/MacOS`. */
  macosEntries: string[];
  /** `lipo -archs` output of the main executable. */
  mainArchs: string;
  /** `otool -L` output of the main executable. */
  mainLinks: string;
  /** `lipo -archs` output per nested Mach-O, keyed by bundle-relative path. */
  nestedArchs: Record<string, string>;
  infoPlist: Record<string, unknown>;
}

export interface VerifierExpectations {
  executable: string;
  version: string;
  identifier: string;
  productName: string;
  /** Files expected under `Contents/Resources/media-control/`, relative to it. */
  mediaControl: string[];
}

const mediaControlPrefix = 'Contents/Resources/media-control/';

export function linkedLibraries(otoolOutput: string): string[] {
  return otoolOutput
    .split('\n')
    .slice(1)
    .map((line) => line.trim().replace(/\s+\(.*\)$/, ''))
    .filter(Boolean);
}

/** Returns every refusal; an empty list means the bundle is acceptable (`RELEASE-01-06`). */
export function verifyBundle(
  inspection: BundleInspection,
  expected: VerifierExpectations,
): string[] {
  const problems: string[] = [];
  const { files, infoPlist } = inspection;

  if (inspection.macosEntries.join(',') !== expected.executable) {
    problems.push(
      `Contents/MacOS must hold only ${expected.executable}, found: ${inspection.macosEntries.join(', ')}`,
    );
  }

  if (inspection.mainArchs.trim() !== 'arm64') {
    problems.push(`main executable architectures are "${inspection.mainArchs.trim()}", not arm64`);
  }

  for (const library of linkedLibraries(inspection.mainLinks)) {
    if (!library.startsWith('/System/') && !library.startsWith('/usr/lib/')) {
      problems.push(`main executable links a non-system library: ${library}`);
    }
  }

  const actualMedia = files
    .filter((file) => file.startsWith(mediaControlPrefix))
    .map((file) => file.slice(mediaControlPrefix.length));
  const expectedMedia = new Set(expected.mediaControl);

  for (const file of actualMedia) {
    if (!expectedMedia.has(file)) {
      problems.push(`unexpected file under media-control: ${file}`);
    }
  }

  for (const file of expectedMedia) {
    if (!actualMedia.includes(file)) {
      problems.push(`missing vendored file under media-control: ${file}`);
    }
  }

  for (const [file, archs] of Object.entries(inspection.nestedArchs)) {
    if (archs.trim() !== 'arm64') {
      problems.push(`nested Mach-O ${file} architectures are "${archs.trim()}", not arm64`);
    }
  }

  for (const file of files) {
    if (file.startsWith(mediaControlPrefix)) {
      continue;
    }

    if (file.includes('.framework/') || file.startsWith('Contents/Frameworks/')) {
      problems.push(`unexpected framework outside media-control: ${file}`);
    }

    const name = file.split('/').at(-1) ?? '';

    if (/^generate-|smoke/i.test(name)) {
      problems.push(`generator or smoke binary in bundle: ${file}`);
    }
  }

  const required: [string, unknown][] = [
    ['LSUIElement', true],
    ['NSHighResolutionCapable', true],
    ['LSMinimumSystemVersion', '26.0'],
    ['CFBundleShortVersionString', expected.version],
    ['CFBundleIdentifier', expected.identifier],
    ['CFBundleName', expected.productName],
  ];

  for (const [key, value] of required) {
    if (infoPlist[key] !== value) {
      problems.push(
        `Info.plist ${key} is ${JSON.stringify(infoPlist[key])}, expected ${JSON.stringify(value)}`,
      );
    }
  }

  if (
    typeof infoPlist.LSApplicationCategoryType !== 'string' ||
    !infoPlist.LSApplicationCategoryType
  ) {
    problems.push('Info.plist lacks LSApplicationCategoryType');
  }

  return problems;
}

function check(result: RunResult, step: string): RunResult {
  if (result.status !== 0) {
    throw new ReleaseError(`${step} failed (exit ${result.status}): ${result.stderr.trim()}`);
  }

  return result;
}

function readPlist(run: Runner, path: string): Record<string, unknown> {
  const output = check(run('plutil', ['-convert', 'json', '-o', '-', path]), `plutil ${path}`);

  return JSON.parse(output.stdout) as Record<string, unknown>;
}

export function inspectBundle(run: Runner, app: string, executable: string): BundleInspection {
  const main = join(app, 'Contents/MacOS', executable);
  const files = listEntries(app);
  const nestedArchs: Record<string, string> = {};

  for (const file of files) {
    if (file.startsWith(mediaControlPrefix) && isMachO(join(app, file))) {
      nestedArchs[file] = check(run('lipo', ['-archs', join(app, file)]), `lipo ${file}`).stdout;
    }
  }

  return {
    files,
    macosEntries: readdirSync(join(app, 'Contents/MacOS')).toSorted(),
    mainArchs: check(run('lipo', ['-archs', main]), 'lipo main executable').stdout,
    mainLinks: check(run('otool', ['-L', main]), 'otool main executable').stdout,
    nestedArchs,
    infoPlist: readPlist(run, join(app, 'Contents/Info.plist')),
  };
}

export interface ReleaseRecord {
  version: string;
  commit: string;
  treeClean: boolean;
  signing: 'ad-hoc';
  notarized: false;
  /** True when the Tauri ad-hoc signature failed strict deep verification and was redone. */
  adHocResigned: boolean;
  artifacts: string[];
}

export interface ReleaseDeps {
  root: string;
  run: Runner;
  env: NodeJS.ProcessEnv;
  log: (line: string) => void;
}

const executableName = 'stache';

/** Re-signs ad hoc inside out (nested Mach-O, main executable, bundle), never with `--deep`. */
function resignAdHoc(run: Runner, app: string, nested: string[], entitlements: string) {
  for (const file of nested) {
    check(
      run('codesign', ['--force', '--sign', '-', '--timestamp=none', join(app, file)]),
      `codesign ${file}`,
    );
  }

  const withEntitlements = [
    '--force',
    '--sign',
    '-',
    '--timestamp=none',
    '--entitlements',
    entitlements,
  ];

  check(
    run('codesign', [...withEntitlements, join(app, 'Contents/MacOS', executableName)]),
    'codesign main executable',
  );
  check(run('codesign', [...withEntitlements, app]), 'codesign bundle');
}

function verifySignature(run: Runner, app: string): RunResult {
  return run('codesign', ['--verify', '--strict', '--deep', '--verbose=2', app]);
}

function checkDmg(run: Runner, dmg: string, names: ArtifactNames, scratch: string) {
  const mount = mkdtempSync(join(scratch, 'mount-'));

  check(
    run('hdiutil', ['attach', '-readonly', '-nobrowse', '-noautoopen', '-mountpoint', mount, dmg]),
    'hdiutil attach',
  );

  try {
    if (!existsSync(join(mount, names.app, 'Contents/Info.plist'))) {
      throw new ReleaseError(`DMG lacks ${names.app}`);
    }

    const link = join(mount, 'Applications');

    if (!lstatSync(link, { throwIfNoEntry: false })?.isSymbolicLink()) {
      throw new ReleaseError('DMG lacks the Applications link');
    }
  } finally {
    const detached = run('hdiutil', ['detach', mount]);

    if (detached.status !== 0) {
      check(run('hdiutil', ['detach', '-force', mount]), 'hdiutil detach');
    }
  }
}

function checkZip(
  run: Runner,
  zip: string,
  names: ArtifactNames,
  rawPlist: Record<string, unknown>,
  scratch: string,
) {
  const expanded = mkdtempSync(join(scratch, 'unzip-'));

  check(run('ditto', ['-x', '-k', zip, expanded]), 'ditto expand');

  const app = join(expanded, names.app);
  const verified = verifySignature(run, app);

  if (verified.status !== 0) {
    throw new ReleaseError(
      `app expanded from the zip fails codesign verification: ${verified.stderr.trim()}`,
    );
  }

  const plist = readPlist(run, join(app, 'Contents/Info.plist'));

  if (JSON.stringify(plist) !== JSON.stringify(rawPlist)) {
    throw new ReleaseError('Info.plist in the zip differs from the raw bundle');
  }
}

/** Builds, verifies, packages and checks one ad-hoc release. Throws `ReleaseError` on any failure. */
export function runRelease({ root, run, env, log }: ReleaseDeps): ReleaseRecord {
  const config = readReleaseConfig(
    readFileSync(join(root, 'src/crates/app/tauri.conf.json'), 'utf8'),
  );
  const version = readWorkspaceVersion(readFileSync(join(root, 'Cargo.toml'), 'utf8'));
  const names = artifactNames(config.productName, config.configVersion);
  const output = join(root, 'target/release-artifacts');
  const staging = join(root, 'target', `release-artifacts.staging-${process.pid}`);
  const git = ['-c', 'core.fsmonitor=false'];

  // A failed run must never leave an earlier run's output looking current.
  rmSync(output, { recursive: true, force: true });
  rmSync(staging, { recursive: true, force: true });

  try {
    const commit = check(
      run('git', [...git, 'rev-parse', 'HEAD'], { cwd: root }),
      'git rev-parse',
    ).stdout.trim();
    const treeClean =
      check(
        run('git', [...git, 'status', '--porcelain'], { cwd: root }),
        'git status',
      ).stdout.trim() === '';

    log('Signing mode: ad-hoc (not notarized). No credential is read.');
    check(
      run('bun', ['run', 'tauri:build'], { cwd: root, env: scrubbedEnv(env), inherit: true }),
      'tauri:build',
    );

    const rawApp = join(root, 'target/release/bundle/macos', names.app);
    const inspection = inspectBundle(run, rawApp, executableName);
    const problems = verifyBundle(inspection, {
      executable: executableName,
      version,
      identifier: config.identifier,
      productName: config.productName,
      mediaControl: listFlattenedResources(join(root, 'src/crates/app/resources/media-control')),
    });

    if (problems.length > 0) {
      throw new ReleaseError(`bundle verifier refused the bundle:\n  ${problems.join('\n  ')}`);
    }

    log('Bundle verifier passed.');
    mkdirSync(staging, { recursive: true });
    const scratch = join(staging, '.work');
    const dmgRoot = join(scratch, 'dmg-root');
    const app = join(dmgRoot, names.app);

    mkdirSync(dmgRoot, { recursive: true });
    check(run('ditto', [rawApp, app]), 'ditto copy');

    let adHocResigned = false;
    const initial = verifySignature(run, app);

    if (initial.status !== 0) {
      log(`Tauri's ad-hoc signature fails strict deep verification:\n${initial.stderr.trim()}`);
      log('Re-signing ad hoc inside out.');
      resignAdHoc(
        run,
        app,
        Object.keys(inspection.nestedArchs),
        join(root, 'src/crates/app/Entitlements.plist'),
      );
      adHocResigned = true;
      const again = verifySignature(run, app);

      if (again.status !== 0) {
        throw new ReleaseError(
          `codesign verification still fails after re-signing: ${again.stderr.trim()}`,
        );
      }
    }

    symlinkSync('/Applications', join(dmgRoot, 'Applications'));
    const dmg = join(staging, names.dmg);
    const zip = join(staging, names.zip);

    check(
      run('hdiutil', [
        'create',
        '-volname',
        names.volume,
        '-srcfolder',
        dmgRoot,
        '-format',
        'UDZO',
        '-ov',
        dmg,
      ]),
      'hdiutil create',
    );
    check(run('ditto', ['-c', '-k', '--keepParent', app, zip]), 'ditto zip');

    checkDmg(run, dmg, names, scratch);
    checkZip(run, zip, names, inspection.infoPlist, scratch);
    log(
      'Post-package checks passed: DMG mounts read-only with the app and Applications link; zip app verifies.',
    );

    rmSync(scratch, { recursive: true, force: true });

    const record: ReleaseRecord = {
      version,
      commit,
      treeClean,
      signing: 'ad-hoc',
      notarized: false,
      adHocResigned,
      artifacts: [names.dmg, names.zip],
    };

    writeFileSync(join(staging, 'release.json'), `${JSON.stringify(record, null, 2)}\n`);

    const sums = check(
      run('shasum', ['-a', '256', names.dmg, names.zip], { cwd: staging }),
      'shasum',
    );
    const lines = sums.stdout.trim().split('\n');

    if (lines.length !== 2 || !lines.every((line) => /^[0-9a-f]{64} {2}\S+$/.test(line))) {
      throw new ReleaseError(`unexpected shasum output: ${sums.stdout}`);
    }

    writeFileSync(join(staging, 'SHA256SUMS'), `${lines.join('\n')}\n`);
    renameSync(staging, output);
    log(
      `Wrote ${output}: ${names.dmg}, ${names.zip}, SHA256SUMS, release.json (ad-hoc, notarized: false).`,
    );

    return record;
  } catch (error) {
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
}
