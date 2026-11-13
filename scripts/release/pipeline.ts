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

import { caskToken, renderCask } from './cask';

/** Variables Tauri reads to sign or notarize; the build child never sees them. */
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

/** The signing input and repository tokens, which the build child must never see. */
export const scrubbedReleaseVariables = [
  'STACHE_SIGNING_IDENTITY',
  'GH_TOKEN',
  'GITHUB_TOKEN',
] as const;

export function scrubbedEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const copy = { ...env };

  for (const name of [...scrubbedAppleVariables, ...scrubbedReleaseVariables]) {
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

/** The only files allowed outside media-control, besides `Contents/MacOS/<executable>`. */
const fixedBundleFiles = [
  'Contents/Info.plist',
  'Contents/Resources/icon.icns',
  'Contents/_CodeSignature/CodeResources',
];

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

  const allowed = new Set([...fixedBundleFiles, `Contents/MacOS/${expected.executable}`]);

  if (!files.includes('Contents/Resources/icon.icns')) {
    problems.push('missing Contents/Resources/icon.icns');
  }

  for (const file of files) {
    if (file.startsWith(mediaControlPrefix)) {
      continue;
    }

    if (!allowed.has(file)) {
      problems.push(`unexpected file in bundle: ${file}`);
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

/** `ad-hoc`: Part 02 pipeline. `self-sign`: the identity named by `STACHE_SIGNING_IDENTITY`. */
export type ReleaseMode = 'ad-hoc' | 'self-sign';

export interface ReleaseRecord {
  version: string;
  commit: string;
  treeClean: boolean;
  signing: 'ad-hoc' | 'self-signed';
  /** Always false: no release is notarized. */
  notarized: false;
  /** True when the Tauri ad-hoc signature failed strict deep verification and was redone. */
  adHocResigned: boolean;
  /** SHA-256 fingerprint of the signing certificate, `self-sign` mode only. */
  certificateSha256?: string;
  artifacts: string[];
}

export interface ReleaseDeps {
  root: string;
  run: Runner;
  env: NodeJS.ProcessEnv;
  log: (line: string) => void;
  mode?: ReleaseMode;
}

const executableName = 'stache';

/** The `--self-sign` input. Not an `APPLE_*` name, so Tauri never reads it. */
export const signingIdentityVariable = 'STACHE_SIGNING_IDENTITY';

export interface SigningIdentity {
  identity: string;
  /** Upper-case hex SHA-256 of the certificate. */
  certificateSha256: string;
}

/**
 * Validates the `--self-sign` identity against the keychain and reads its certificate
 * fingerprint. Throws on anything missing or wrong; never falls back to ad hoc.
 */
export function readSigningIdentity(env: NodeJS.ProcessEnv, run: Runner): SigningIdentity {
  const identity = env[signingIdentityVariable]?.trim() ?? '';

  if (!identity) {
    throw new ReleaseError(`--self-sign requires ${signingIdentityVariable}`);
  }

  if (identity === '-') {
    throw new ReleaseError(`${signingIdentityVariable} "-" is ad hoc, not a certificate`);
  }

  const listed = check(
    run('security', ['find-identity', '-p', 'codesigning']),
    'security find-identity',
  );

  if (!listed.stdout.includes(`"${identity}"`)) {
    throw new ReleaseError(`no code-signing identity named "${identity}" in the keychain`);
  }

  const found = check(
    run('security', ['find-certificate', '-a', '-c', identity, '-Z']),
    'security find-certificate',
  );
  const hashes = new Set(
    [...found.stdout.matchAll(/^SHA-256 hash: ([0-9A-Fa-f]{64})$/gm)].map((match) =>
      match[1].toUpperCase(),
    ),
  );

  if (hashes.size !== 1) {
    throw new ReleaseError(`expected one certificate matching "${identity}", found ${hashes.size}`);
  }

  return { identity, certificateSha256: [...hashes][0] };
}

/**
 * Signs inside out (each nested Mach-O, main executable, bundle), never with `--deep`.
 * Entitlements go only on the main executable and the bundle.
 */
function signApp(
  run: Runner,
  app: string,
  nested: string[],
  entitlements: string,
  identity: string,
  runtime: boolean,
) {
  const base = [
    '--force',
    ...(runtime ? ['--options', 'runtime'] : []),
    '--timestamp=none',
    '--sign',
    identity,
  ];

  for (const file of nested) {
    check(run('codesign', [...base, join(app, file)]), `codesign ${file}`);
  }

  const withEntitlements = [...base, '--entitlements', entitlements];

  check(
    run('codesign', [...withEntitlements, join(app, 'Contents/MacOS', executableName)]),
    'codesign main executable',
  );
  check(run('codesign', [...withEntitlements, app]), 'codesign bundle');
}

/** Checks the authority, the hardened-runtime flag and a certificate-anchored requirement. */
function checkSelfSignature(run: Runner, path: string, identity: string) {
  const shown = check(run('codesign', ['-dvvv', path]), `codesign -dvvv ${path}`);
  const text = `${shown.stdout}\n${shown.stderr}`;

  if (!/flags=0x[0-9a-f]+\([^)]*\bruntime\b/.test(text)) {
    throw new ReleaseError(`${path} lacks the hardened-runtime flag`);
  }

  if (!text.split('\n').includes(`Authority=${identity}`)) {
    throw new ReleaseError(`${path} is not signed by "${identity}"`);
  }

  const requirement = check(run('codesign', ['-d', '-r-', path]), `codesign -d -r- ${path}`);
  const designated = `${requirement.stdout}\n${requirement.stderr}`;

  if (!designated.includes('certificate leaf') || designated.includes('cdhash')) {
    throw new ReleaseError(
      `${path} designated requirement is not anchored on the certificate: ${designated.trim()}`,
    );
  }
}

function verifySignature(run: Runner, app: string): RunResult {
  return run('codesign', ['--verify', '--strict', '--deep', '--verbose=2', app]);
}

function checkDmg(
  run: Runner,
  dmg: string,
  names: ArtifactNames,
  scratch: string,
  checkApp?: (app: string) => void,
) {
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

    checkApp?.(join(mount, names.app));
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
  checkApp?: (app: string) => void,
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

  checkApp?.(app);
}

/**
 * Removes `release-artifacts.staging-*` left by an interrupted run, detaching any image still
 * mounted under one first. Best effort: never fails the run.
 */
function sweepStaleStaging(run: Runner, target: string, log: (line: string) => void) {
  const stale = existsSync(target)
    ? readdirSync(target).filter((name) => name.startsWith('release-artifacts.staging-'))
    : [];

  for (const name of stale) {
    const work = join(target, name, '.work');
    const mounts = existsSync(work)
      ? readdirSync(work).filter((entry) => entry.startsWith('mount-'))
      : [];

    for (const mount of mounts) {
      try {
        run('hdiutil', ['detach', '-force', join(work, mount)]);
      } catch {
        // Best effort.
      }
    }

    try {
      rmSync(join(target, name), { recursive: true, force: true });
      log(`Removed stale ${name} from an interrupted run.`);
    } catch {
      log(`Could not remove stale ${name}; continuing.`);
    }
  }
}

/**
 * Builds, verifies, signs (per mode), packages and checks one release. Throws `ReleaseError` on
 * any failure, leaving no `SHA256SUMS`.
 */
export function runRelease({ root, run, env, log, mode = 'ad-hoc' }: ReleaseDeps): ReleaseRecord {
  const config = readReleaseConfig(
    readFileSync(join(root, 'src/crates/app/tauri.conf.json'), 'utf8'),
  );
  const version = readWorkspaceVersion(readFileSync(join(root, 'Cargo.toml'), 'utf8'));
  const names = artifactNames(config.productName, config.configVersion);
  const output = join(root, 'target/release-artifacts');
  const staging = join(root, 'target', `release-artifacts.staging-${process.pid}`);
  const entitlements = join(root, 'src/crates/app/Entitlements.plist');
  const git = ['-c', 'core.fsmonitor=false'];

  sweepStaleStaging(run, join(root, 'target'), log);
  // A failed run must never leave an earlier run's output looking current.
  rmSync(output, { recursive: true, force: true });
  rmSync(staging, { recursive: true, force: true });

  // Refuse a missing or unknown `--self-sign` identity before building; never fall back.
  const signer = mode === 'self-sign' ? readSigningIdentity(env, run) : null;

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

    log(
      signer
        ? `Signing mode: self-signed with "${signer.identity}" (certificate SHA-256 ${signer.certificateSha256}, not notarized).`
        : 'Signing mode: ad-hoc (not notarized). No credential is read.',
    );
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
    const nested = Object.keys(inspection.nestedArchs);

    mkdirSync(dmgRoot, { recursive: true });
    check(run('ditto', [rawApp, app]), 'ditto copy');

    let adHocResigned = false;
    const verifyStrict = (path: string, label: string) => {
      const verified = verifySignature(run, path);

      if (verified.status !== 0) {
        throw new ReleaseError(`${label} fails codesign verification: ${verified.stderr.trim()}`);
      }
    };

    if (signer) {
      signApp(run, app, nested, entitlements, signer.identity, true);
      verifyStrict(app, 'signed app');
      checkSelfSignature(run, app, signer.identity);
      log(`Signed and verified the app with "${signer.identity}".`);
    } else {
      const initial = verifySignature(run, app);

      if (initial.status !== 0) {
        log(`Tauri's ad-hoc signature fails strict deep verification:\n${initial.stderr.trim()}`);
        log('Re-signing ad hoc inside out.');
        signApp(run, app, nested, entitlements, '-', false);
        adHocResigned = true;
        const again = verifySignature(run, app);

        if (again.status !== 0) {
          throw new ReleaseError(
            `codesign verification still fails after re-signing: ${again.stderr.trim()}`,
          );
        }
      }
    }

    // DMG and zip from the signed app.
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

    if (signer) {
      check(
        run('codesign', ['--force', '--timestamp=none', '--sign', signer.identity, dmg]),
        'codesign DMG',
      );
    }

    check(run('ditto', ['-c', '-k', '--keepParent', app, zip]), 'ditto zip');

    // Final verification on the artifacts.
    const checkSigned = signer
      ? (packaged: string) => checkSelfSignature(run, packaged, signer.identity)
      : undefined;

    checkDmg(
      run,
      dmg,
      names,
      scratch,
      checkSigned &&
        ((mounted) => {
          verifyStrict(mounted, 'app in the DMG');
          checkSigned(mounted);
        }),
    );
    checkZip(run, zip, names, inspection.infoPlist, scratch, checkSigned);
    log(
      'Post-package checks passed: DMG mounts read-only with the app and Applications link; zip app verifies.',
    );

    rmSync(scratch, { recursive: true, force: true });

    // Step 6: SHA256SUMS over the final bytes, then release.json.
    const sums = check(
      run('shasum', ['-a', '256', names.dmg, names.zip], { cwd: staging }),
      'shasum',
    );
    const lines = sums.stdout.trim().split('\n');

    if (lines.length !== 2 || !lines.every((line) => /^[0-9a-f]{64} {2}\S+$/.test(line))) {
      throw new ReleaseError(`unexpected shasum output: ${sums.stdout}`);
    }

    writeFileSync(join(staging, 'SHA256SUMS'), `${lines.join('\n')}\n`);

    // The cask, from the authoritative version and the DMG's real checksum. Not an artifact:
    // it stays out of SHA256SUMS and release.json, and Part 06 publishes it by hand.
    mkdirSync(join(staging, 'Casks'));
    writeFileSync(
      join(staging, 'Casks', `${caskToken}.rb`),
      renderCask({
        productName: config.productName,
        identifier: config.identifier,
        version,
        sha256: (lines.find((line) => line.endsWith(`  ${names.dmg}`)) ?? '').split('  ')[0],
      }),
    );

    const record: ReleaseRecord = {
      version,
      commit,
      treeClean,
      signing: signer ? 'self-signed' : 'ad-hoc',
      notarized: false,
      adHocResigned,
      ...(signer ? { certificateSha256: signer.certificateSha256 } : {}),
      artifacts: [names.dmg, names.zip],
    };

    writeFileSync(join(staging, 'release.json'), `${JSON.stringify(record, null, 2)}\n`);
    renameSync(staging, output);
    log(
      `Wrote ${output}: ${names.dmg}, ${names.zip}, SHA256SUMS, release.json, Casks/${caskToken}.rb (${record.signing}, notarized: ${record.notarized}).`,
    );

    return record;
  } catch (error) {
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
}
