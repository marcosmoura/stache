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

/**
 * `ad-hoc`: Part 02 pipeline. `dry-run-signing`: the Part 03 step order with identity `-`,
 * hardened runtime and notarization/stapling skipped. `sign`: Developer ID and notarization.
 */
export type ReleaseMode = 'ad-hoc' | 'dry-run-signing' | 'sign';

export interface ReleaseRecord {
  version: string;
  commit: string;
  treeClean: boolean;
  signing: 'ad-hoc' | 'developer-id';
  notarized: boolean;
  /** True when the Tauri ad-hoc signature failed strict deep verification and was redone. */
  adHocResigned: boolean;
  /** Present only for the Part 03 modes. */
  mode?: Exclude<ReleaseMode, 'ad-hoc'>;
  /** notarytool submission identifiers by artifact, `sign` mode only. */
  notarySubmissions?: Record<string, string>;
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

/**
 * The script's own `--sign` inputs. They are deliberately not `APPLE_*`: those names are what
 * Tauri reads and what the build child has scrubbed, so these never reach Tauri.
 */
export const signingVariables = {
  identity: 'STACHE_SIGNING_IDENTITY',
  teamId: 'STACHE_TEAM_ID',
  keyId: 'STACHE_NOTARY_KEY_ID',
  issuer: 'STACHE_NOTARY_ISSUER',
  keyPath: 'STACHE_NOTARY_KEY_PATH',
  profile: 'STACHE_NOTARY_KEYCHAIN_PROFILE',
} as const;

export interface SigningInputs {
  identity: string;
  teamId: string;
  /** notarytool credential arguments; never logged or recorded. */
  notary: string[];
}

/** Validates the `--sign` inputs. Throws on anything missing or wrong; never falls back. */
export function readSigningInputs(env: NodeJS.ProcessEnv): SigningInputs {
  const value = (name: string) => env[name]?.trim() ?? '';
  const v = signingVariables;
  const identity = value(v.identity);
  const teamId = value(v.teamId);

  if (!identity || !teamId) {
    throw new ReleaseError(`--sign requires ${v.identity} and ${v.teamId}`);
  }

  if (!/^[A-Z0-9]{10}$/.test(teamId)) {
    throw new ReleaseError(`${v.teamId} is not a ten-character team identifier`);
  }

  const match = /^Developer ID Application: .+ \(([A-Z0-9]{10})\)$/.exec(identity);

  if (!match) {
    throw new ReleaseError(
      `${v.identity} must be a "Developer ID Application: <name> (<team>)" identity`,
    );
  }

  if (match[1] !== teamId) {
    throw new ReleaseError(`${v.identity} belongs to team ${match[1]}, not ${v.teamId}`);
  }

  const api = [value(v.keyId), value(v.issuer), value(v.keyPath)];
  const profile = value(v.profile);
  const apiSet = api.filter(Boolean).length;

  if (apiSet > 0 && apiSet < 3) {
    throw new ReleaseError(`set all of ${v.keyId}, ${v.issuer} and ${v.keyPath}`);
  }

  if ((apiSet === 3) === Boolean(profile)) {
    throw new ReleaseError(
      `--sign requires exactly one notarytool credential: the API key (${v.keyId}, ${v.issuer}, ${v.keyPath}) or ${v.profile}`,
    );
  }

  const notary =
    apiSet === 3
      ? ['--key', api[2], '--key-id', api[0], '--issuer', api[1]]
      : ['--keychain-profile', profile];

  return { identity, teamId, notary };
}

interface SignOptions {
  identity: string;
  /** `--timestamp` or `--timestamp=none`. */
  timestamp: string;
  runtime: boolean;
}

/** Signs inside out (each nested Mach-O, main executable, bundle), never with `--deep`. */
function signApp(
  run: Runner,
  app: string,
  nested: string[],
  entitlements: string,
  { identity, timestamp, runtime }: SignOptions,
) {
  const base = ['--force', '--sign', identity, ...(runtime ? ['--options', 'runtime'] : [])];

  for (const file of nested) {
    check(run('codesign', [...base, timestamp, join(app, file)]), `codesign ${file}`);
  }

  const withEntitlements = [...base, timestamp, '--entitlements', entitlements];

  check(
    run('codesign', [...withEntitlements, join(app, 'Contents/MacOS', executableName)]),
    'codesign main executable',
  );
  check(run('codesign', [...withEntitlements, app]), 'codesign bundle');
}

/** Checks `codesign -dvvv`; Developer ID details only when `inputs` is given. */
function checkSignatureDetails(
  run: Runner,
  path: string,
  inputs: SigningInputs | null,
  log: (line: string) => void,
) {
  const shown = check(run('codesign', ['-dvvv', path]), `codesign -dvvv ${path}`);
  const text = `${shown.stdout}\n${shown.stderr}`;

  if (!/flags=0x[0-9a-f]+\([^)]*\bruntime\b/.test(text)) {
    throw new ReleaseError(`${path} lacks the hardened-runtime flag`);
  }

  if (!inputs) {
    log(`${path}: hardened runtime present; authority, team and timestamp skipped (dry run).`);

    return;
  }

  const expected = [
    `Authority=${inputs.identity}`,
    `TeamIdentifier=${inputs.teamId}`,
    'Timestamp=',
  ];

  for (const line of expected) {
    if (!text.split('\n').some((shownLine) => shownLine.startsWith(line))) {
      throw new ReleaseError(`${path} signature lacks ${line}`);
    }
  }
}

function staple(
  run: Runner,
  path: string,
  label: string,
  inputs: SigningInputs | null,
  log: (line: string) => void,
) {
  if (!inputs) {
    log(`Stapling the ${label}: skipped (dry run).`);

    return;
  }

  check(run('xcrun', ['stapler', 'staple', path]), `stapler staple ${label}`);
  check(run('xcrun', ['stapler', 'validate', path]), `stapler validate ${label}`);
}

/** Submits and waits; on anything but `Accepted`, saves the notarytool log as evidence and fails. */
function notarize(
  run: Runner,
  path: string,
  label: string,
  inputs: SigningInputs | null,
  evidence: string,
  log: (line: string) => void,
): string | undefined {
  if (!inputs) {
    log(`Notarization of the ${label}: skipped (dry run, not notarized).`);

    return undefined;
  }

  const submitted = run('xcrun', [
    'notarytool',
    'submit',
    path,
    '--wait',
    '--output-format',
    'json',
    ...inputs.notary,
  ]);
  let id: string | undefined;
  let status: string | undefined;

  try {
    ({ id, status } = JSON.parse(submitted.stdout) as { id?: string; status?: string });
  } catch {
    // Leave both unset: treated as a failed submission below.
  }

  if (submitted.status === 0 && status === 'Accepted' && id) {
    log(`Notarization of the ${label}: Accepted (submission ${id}).`);

    return id;
  }

  let saved = '';

  if (id) {
    mkdirSync(evidence, { recursive: true });
    const logPath = join(evidence, `notary-${label}-${id}.json`);
    const fetched = run('xcrun', ['notarytool', 'log', id, ...inputs.notary, logPath]);

    saved = fetched.status === 0 ? `; log saved to ${logPath}` : '; fetching its log also failed';
  }

  throw new ReleaseError(
    `notarization of the ${label} returned ${status ?? `exit ${submitted.status}`}${id ? ` (submission ${id})` : ''}${saved}`,
  );
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
 * Builds, verifies, signs (per mode), packages and checks one release. Throws `ReleaseError` on
 * any failure, leaving no `SHA256SUMS` and never a `notarized: true` record.
 */
export function runRelease({ root, run, env, log, mode = 'ad-hoc' }: ReleaseDeps): ReleaseRecord {
  // Refuse missing or wrong `--sign` inputs before touching anything; never fall back.
  const inputs = mode === 'sign' ? readSigningInputs(env) : null;
  const config = readReleaseConfig(
    readFileSync(join(root, 'src/crates/app/tauri.conf.json'), 'utf8'),
  );
  const version = readWorkspaceVersion(readFileSync(join(root, 'Cargo.toml'), 'utf8'));
  const names = artifactNames(config.productName, config.configVersion);
  const output = join(root, 'target/release-artifacts');
  const evidence = join(root, 'target/release-evidence');
  const staging = join(root, 'target', `release-artifacts.staging-${process.pid}`);
  const entitlements = join(root, 'src/crates/app/Entitlements.plist');
  const git = ['-c', 'core.fsmonitor=false'];

  // A failed run must never leave an earlier run's output looking current.
  rmSync(output, { recursive: true, force: true });
  rmSync(evidence, { recursive: true, force: true });
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

    if (inputs && !treeClean) {
      throw new ReleaseError('--sign requires a clean git tree');
    }

    log(
      {
        'ad-hoc': 'Signing mode: ad-hoc (not notarized). No credential is read.',
        'dry-run-signing':
          'Signing mode: dry run (identity "-", hardened runtime, not notarized). No credential is read.',
        sign: `Signing mode: Developer ID (team ${inputs?.teamId}).`,
      }[mode],
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
    const notarySubmissions: Record<string, string> = {};
    const signing = mode !== 'ad-hoc';
    const options: SignOptions = {
      identity: inputs?.identity ?? '-',
      timestamp: inputs ? '--timestamp' : '--timestamp=none',
      runtime: signing,
    };
    const verifyStrict = (path: string, label: string) => {
      const verified = verifySignature(run, path);

      if (verified.status !== 0) {
        throw new ReleaseError(`${label} fails codesign verification: ${verified.stderr.trim()}`);
      }
    };

    if (signing) {
      // Step 1: sign inside out and verify.
      signApp(run, app, nested, entitlements, options);
      verifyStrict(app, 'signed app');
      checkSignatureDetails(run, app, inputs, log);

      // Step 2: notarize a ditto zip of the app.
      const submission = join(scratch, `submit-${names.zip}`);

      check(run('ditto', ['-c', '-k', '--keepParent', app, submission]), 'ditto submission zip');
      const appId = notarize(run, submission, 'app', inputs, evidence, log);

      if (appId) {
        notarySubmissions.app = appId;
      }

      // Step 3: staple the app and validate.
      staple(run, app, 'app', inputs, log);
    } else {
      const initial = verifySignature(run, app);

      if (initial.status !== 0) {
        log(`Tauri's ad-hoc signature fails strict deep verification:\n${initial.stderr.trim()}`);
        log('Re-signing ad hoc inside out.');
        signApp(run, app, nested, entitlements, options);
        adHocResigned = true;
        const again = verifySignature(run, app);

        if (again.status !== 0) {
          throw new ReleaseError(
            `codesign verification still fails after re-signing: ${again.stderr.trim()}`,
          );
        }
      }
    }

    // Step 4: DMG from the (stapled) app.
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

    if (signing) {
      check(
        run('codesign', ['--force', '--sign', options.identity, options.timestamp, dmg]),
        'codesign DMG',
      );
      const dmgId = notarize(run, dmg, 'dmg', inputs, evidence, log);

      if (dmgId) {
        notarySubmissions.dmg = dmgId;
      }

      staple(run, dmg, 'dmg', inputs, log);
    }

    // Step 5: distributable zip from the stapled app.
    check(run('ditto', ['-c', '-k', '--keepParent', app, zip]), 'ditto zip');

    // Final verification on the artifacts.
    const checkPackagedApp = signing
      ? (packaged: string) => {
          checkSignatureDetails(run, packaged, inputs, log);

          if (inputs) {
            check(run('xcrun', ['stapler', 'validate', packaged]), 'stapler validate packaged app');
          }
        }
      : undefined;

    if (signing) {
      verifyStrict(app, 'packaged app');
      checkSignatureDetails(run, app, inputs, log);

      if (inputs) {
        check(run('spctl', ['--assess', '--type', 'execute', '-vv', app]), 'spctl assess app');
        check(
          run('spctl', [
            '--assess',
            '--type',
            'open',
            '--context',
            'context:primary-signature',
            '-vv',
            dmg,
          ]),
          'spctl assess DMG',
        );
        check(run('xcrun', ['stapler', 'validate', dmg]), 'stapler validate DMG');
      } else {
        log('spctl assessment and stapler validation: skipped (dry run).');
      }
    }

    checkDmg(run, dmg, names, scratch, checkPackagedApp);
    checkZip(run, zip, names, inspection.infoPlist, scratch, checkPackagedApp);
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

    const record: ReleaseRecord = {
      version,
      commit,
      treeClean,
      signing: inputs ? 'developer-id' : 'ad-hoc',
      notarized: inputs !== null,
      adHocResigned,
      ...(mode === 'ad-hoc' ? {} : { mode }),
      ...(inputs ? { notarySubmissions } : {}),
      artifacts: [names.dmg, names.zip],
    };

    writeFileSync(join(staging, 'release.json'), `${JSON.stringify(record, null, 2)}\n`);
    renameSync(staging, output);
    log(
      `Wrote ${output}: ${names.dmg}, ${names.zip}, SHA256SUMS, release.json (${record.signing}, notarized: ${record.notarized}).`,
    );

    return record;
  } catch (error) {
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
}
