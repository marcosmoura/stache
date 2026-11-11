/**
 * Vendored media adapter provenance (`bun run vendor:media-control -- <mode>`).
 *
 * `--check` (default, offline) compares `src/crates/app/resources/media-control/` with
 * `manifest.json`. `--verify-upstream` downloads the pinned Homebrew bottle by digest and the
 * pinned upstream files and compares them byte for byte. `--refresh --bottle <tag> --version <v>`
 * rewrites the files and the manifest from a new bottle. Downloaded data is never executed: it is
 * hashed before extraction, its listing is validated, and its temporary directory is removed.
 */
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  chmodSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { gunzipSync } from 'node:zlib';

export interface ManifestFile {
  path: string;
  bottlePath: string;
  upstreamPath?: string;
  sha256: string;
  mode: string;
  machO: boolean;
}

export interface ManifestSymlink {
  path: string;
  bottlePath: string;
  target: string;
}

export interface Manifest {
  formula: {
    name: string;
    version: string;
    bottleTag: string;
    blobDigest: string;
    repository: string;
  };
  upstream: {
    mediaControl: { repository: string; tag: string; revision: string };
    mediaremoteAdapter: { repository: string; tag: string; revision: string };
  };
  files: ManifestFile[];
  symlinks: ManifestSymlink[];
  license: { path: string; upstreamPath: string; sha256: string };
}

export type Fetch = (url: string, init?: RequestInit) => Promise<Response>;

export const REPO_ROOT = path.resolve(import.meta.dirname, '..', '..');
export const RESOURCE_DIR = path.join(REPO_ROOT, 'src/crates/app/resources/media-control');
export const MANIFEST_PATH = path.join(import.meta.dirname, 'manifest.json');

/** Files allowed in the resource directory besides the manifest's entries. */
const DOCUMENTS = ['PROVENANCE.md'];
const USER_AGENT = 'stache-vendor-media-control';

export function readManifest(file = MANIFEST_PATH): Manifest {
  return JSON.parse(readFileSync(file, 'utf8')) as Manifest;
}

export function sha256(buffer: Buffer): string {
  return createHash('sha256').update(buffer).digest('hex');
}

const MACH_O_MAGICS = new Set([
  'feedface',
  'cefaedfe',
  'feedfacf',
  'cffaedfe',
  'cafebabe',
  'bebafeca',
  'cafebabf',
  'bfbafeca',
]);

/** Whether the bytes start with any Mach-O or fat (universal) magic. */
export function looksLikeMachO(bytes: Buffer): boolean {
  return bytes.length >= 4 && MACH_O_MAGICS.has(bytes.subarray(0, 4).toString('hex'));
}

/** Null when `bytes` is a thin 64-bit little-endian arm64 Mach-O, otherwise the reason. */
export function thinArm64Problem(bytes: Buffer): string | null {
  if (bytes.length < 8 || !looksLikeMachO(bytes)) {
    return 'is not a Mach-O image';
  }

  if (bytes.subarray(0, 4).toString('hex') !== 'cffaedfe') {
    return 'is not a thin 64-bit Mach-O (fat or 32-bit header)';
  }

  if (bytes.readUInt32LE(4) !== 0x0100000c) {
    return `has CPU type 0x${bytes.readUInt32LE(4).toString(16)}, not arm64`;
  }

  return null;
}

function isExecutable(mode: number): boolean {
  return (mode & 0o111) !== 0;
}

function manifestExecutable(file: ManifestFile): boolean {
  return file.mode === '100755';
}

/** Offline check of `root` against `manifest`. Returns one message per problem, each naming a path. */
export function checkDirectory(root: string, manifest: Manifest): string[] {
  const errors: string[] = [];
  const expected = new Set<string>([...DOCUMENTS, manifest.license.path]);

  for (const entry of [...manifest.files, ...manifest.symlinks]) {
    expected.add(entry.path);

    for (let dir = path.posix.dirname(entry.path); dir !== '.'; dir = path.posix.dirname(dir)) {
      expected.add(dir);
    }
  }

  for (const file of manifest.files) {
    const full = path.join(root, file.path);
    let stat;

    try {
      stat = lstatSync(full);
    } catch {
      errors.push(`${file.path}: missing`);
      continue;
    }

    if (!stat.isFile()) {
      errors.push(`${file.path}: not a regular file`);
      continue;
    }

    const bytes = readFileSync(full);

    if (sha256(bytes) !== file.sha256) {
      errors.push(`${file.path}: SHA-256 ${sha256(bytes)} differs from manifest ${file.sha256}`);
    }

    if (isExecutable(stat.mode) !== manifestExecutable(file)) {
      errors.push(
        `${file.path}: executable bit is ${isExecutable(stat.mode) ? 'set' : 'unset'}, manifest mode ${file.mode}`,
      );
    }

    if (file.machO || looksLikeMachO(bytes)) {
      const problem = thinArm64Problem(bytes);

      if (problem) {
        errors.push(`${file.path}: ${problem}`);
      }
    }
  }

  for (const link of manifest.symlinks) {
    const full = path.join(root, link.path);
    let stat;

    try {
      stat = lstatSync(full);
    } catch {
      errors.push(`${link.path}: missing symlink`);
      continue;
    }

    if (!stat.isSymbolicLink()) {
      errors.push(`${link.path}: not a symlink`);
      continue;
    }

    const target = readlinkSync(full);

    if (target !== link.target) {
      errors.push(`${link.path}: symlink points to ${target}, manifest ${link.target}`);
    }
  }

  const licensePath = path.join(root, manifest.license.path);

  try {
    const actual = sha256(readFileSync(licensePath));

    if (actual !== manifest.license.sha256) {
      errors.push(`${manifest.license.path}: SHA-256 ${actual} differs from manifest`);
    }
  } catch {
    errors.push(`${manifest.license.path}: missing`);
  }

  const walk = (relative: string): void => {
    for (const entry of readdirSync(path.join(root, relative), { withFileTypes: true })) {
      const child = relative ? `${relative}/${entry.name}` : entry.name;

      if (!expected.has(child)) {
        errors.push(
          `${child}: unexpected entry in the resource directory (not in the manifest; delete it if it is a tool artifact such as an empty .claude/)`,
        );
        continue;
      }

      if (entry.isDirectory()) {
        walk(child);
      }
    }
  };

  walk('');

  return errors;
}

export interface TarEntry {
  name: string;
  type: string;
  linkName: string;
}

function cString(block: Buffer, start: number, length: number): string {
  const field = block.subarray(start, start + length);
  const end = field.indexOf(0);

  return field.subarray(0, end === -1 ? length : end).toString('utf8');
}

function parsePax(data: Buffer): Map<string, string> {
  const records = new Map<string, string>();
  let offset = 0;

  while (offset < data.length) {
    const space = data.indexOf(0x20, offset);

    if (space === -1) {
      break;
    }

    const length = Number.parseInt(data.subarray(offset, space).toString('utf8'), 10);

    if (!Number.isFinite(length) || length <= 0) {
      throw new Error('Malformed pax header');
    }

    const record = data.subarray(space + 1, offset + length - 1).toString('utf8');
    const equals = record.indexOf('=');

    records.set(record.slice(0, equals), record.slice(equals + 1));
    offset += length;
  }

  return records;
}

/** Lists a gzipped tar archive without extracting it (ustar, pax and GNU long names). */
export function listTarGz(archive: Buffer): TarEntry[] {
  const data = gunzipSync(archive);
  const entries: TarEntry[] = [];
  let offset = 0;
  let pending: { path?: string; linkpath?: string } = {};

  while (offset + 512 <= data.length) {
    const header = data.subarray(offset, offset + 512);

    if (header.every((byte) => byte === 0)) {
      break;
    }

    const size = Number.parseInt(cString(header, 124, 12).trim() || '0', 8);

    // A size this lister cannot read (for example base-256) would end the listing early and hide
    // the entries after it from validation.
    if (!Number.isFinite(size) || size < 0) {
      throw new Error('Malformed tar header: unreadable entry size');
    }

    const type = String.fromCharCode(header[156] || 0x30);
    const body = data.subarray(offset + 512, offset + 512 + size);
    const prefix = cString(header, 345, 155);
    const baseName = cString(header, 0, 100);

    offset += 512 + Math.ceil(size / 512) * 512;

    if (type === 'x') {
      const records = parsePax(body);

      pending = { path: records.get('path'), linkpath: records.get('linkpath') };
      continue;
    }

    if (type === 'g') {
      continue;
    }

    if (type === 'L') {
      pending.path = cString(body, 0, body.length);
      continue;
    }

    if (type === 'K') {
      pending.linkpath = cString(body, 0, body.length);
      continue;
    }

    entries.push({
      name: pending.path ?? (prefix ? `${prefix}/${baseName}` : baseName),
      type,
      linkName: pending.linkpath ?? cString(header, 157, 100),
    });
    pending = {};
  }

  return entries;
}

/** Problems that make the listing unsafe to extract: absolute or `..` paths, escaping links. */
export function tarListingProblems(entries: TarEntry[]): string[] {
  const problems: string[] = [];

  for (const entry of entries) {
    if (entry.name.startsWith('/') || entry.name.split('/').includes('..')) {
      problems.push(`${entry.name}: archive entry escapes the extraction directory`);
      continue;
    }

    if (!['0', '5', '2'].includes(entry.type)) {
      problems.push(`${entry.name}: unsupported archive entry type '${entry.type}'`);
      continue;
    }

    if (entry.type === '2') {
      const resolved = path.posix.normalize(
        path.posix.join(path.posix.dirname(entry.name), entry.linkName),
      );

      if (
        entry.linkName.startsWith('/') ||
        resolved === '..' ||
        resolved.startsWith('../') ||
        resolved.startsWith('/')
      ) {
        problems.push(`${entry.name}: symlink target ${entry.linkName} leaves the directory`);
      }
    }
  }

  return problems;
}

async function fetchOk(fetchFn: Fetch, url: string, headers: Record<string, string> = {}) {
  const response = await fetchFn(url, { headers: { 'User-Agent': USER_AGENT, ...headers } });

  if (!response.ok) {
    throw new Error(`Download failed (${response.status}): ${url}`);
  }

  return response;
}

/** Downloads a bottle blob from ghcr.io with an anonymous pull token. */
export async function downloadBottle(
  fetchFn: Fetch,
  repository: string,
  digest: string,
): Promise<Buffer> {
  const tokenUrl = `https://ghcr.io/token?service=ghcr.io&scope=repository:${repository}:pull`;
  const { token } = (await (await fetchOk(fetchFn, tokenUrl)).json()) as { token: string };
  const blobUrl = `https://ghcr.io/v2/${repository}/blobs/${digest}`;
  const response = await fetchOk(fetchFn, blobUrl, { Authorization: `Bearer ${token}` });

  return Buffer.from(await response.arrayBuffer());
}

export function upstreamUrl(manifest: Manifest, file: string): string {
  const { repository, revision } = manifest.upstream.mediaremoteAdapter;

  return `https://raw.githubusercontent.com/${repository}/${revision}/${file}`;
}

/**
 * Verifies `archive` against `digest` before anything touches the disk, validates its listing,
 * extracts it with the system `tar` into a fresh directory under `tmpRoot`, runs `inspect` on the
 * extraction root and always removes the directory.
 */
export function withVerifiedBottle<T>(
  archive: Buffer,
  digest: string,
  tmpRoot: string,
  inspect: (extracted: string) => T,
): T {
  const actual = `sha256:${sha256(archive)}`;

  if (actual !== digest) {
    throw new Error(`Bottle digest ${actual} differs from manifest ${digest}; nothing extracted`);
  }

  const problems = tarListingProblems(listTarGz(archive));

  if (problems.length > 0) {
    throw new Error(`Unsafe bottle archive:\n${problems.join('\n')}`);
  }

  const workspace = mkdtempSync(path.join(tmpRoot, 'media-control-bottle-'));

  try {
    const archivePath = path.join(workspace, 'bottle.tar.gz');
    const extracted = path.join(workspace, 'out');

    mkdirSync(extracted);
    writeFileSync(archivePath, archive);
    execFileSync('tar', ['-xzf', archivePath, '-C', extracted], { cwd: tmpRoot });

    return inspect(extracted);
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
}

/** Compares the extracted bottle with the committed files. */
export function compareWithBottle(extracted: string, root: string, manifest: Manifest): string[] {
  const errors: string[] = [];

  for (const file of manifest.files) {
    let bottle: Buffer;

    try {
      bottle = readFileSync(path.join(extracted, file.bottlePath));
    } catch {
      errors.push(`${file.path}: missing from the bottle at ${file.bottlePath}`);
      continue;
    }

    if (sha256(bottle) !== file.sha256) {
      errors.push(`${file.path}: bottle file SHA-256 ${sha256(bottle)} differs from manifest`);
    }

    let committed: Buffer | null = null;

    try {
      committed = readFileSync(path.join(root, file.path));
    } catch {
      errors.push(`${file.path}: missing from the resource directory`);
    }

    if (committed && !committed.equals(bottle)) {
      errors.push(`${file.path}: differs from the bottle's ${file.bottlePath}`);
    }

    const bottleExecutable = isExecutable(lstatSync(path.join(extracted, file.bottlePath)).mode);

    if (bottleExecutable !== manifestExecutable(file)) {
      errors.push(`${file.path}: executable bit differs from the bottle`);
    }
  }

  for (const link of manifest.symlinks) {
    let bottleTarget: string | null = null;
    let committedTarget: string | null = null;

    try {
      bottleTarget = readlinkSync(path.join(extracted, link.bottlePath));
    } catch {
      errors.push(`${link.path}: symlink missing from the bottle`);
    }

    try {
      committedTarget = readlinkSync(path.join(root, link.path));
    } catch {
      errors.push(`${link.path}: symlink missing from the resource directory`);
    }

    if (bottleTarget !== null && bottleTarget !== link.target) {
      errors.push(
        `${link.path}: bottle symlink points to ${bottleTarget}, manifest ${link.target}`,
      );
    }

    if (committedTarget !== null && committedTarget !== link.target) {
      errors.push(`${link.path}: symlink points to ${committedTarget}, manifest ${link.target}`);
    }
  }

  return errors;
}

/** Compares the committed script and `LICENSE` with upstream at the pinned revision. */
export async function compareWithUpstream(
  fetchFn: Fetch,
  root: string,
  manifest: Manifest,
): Promise<string[]> {
  const errors: string[] = [];
  const pairs = [
    ...manifest.files
      .filter((file) => file.upstreamPath)
      .map((file) => ({ local: file.path, remote: file.upstreamPath as string })),
    { local: manifest.license.path, remote: manifest.license.upstreamPath },
  ];

  for (const pair of pairs) {
    const url = upstreamUrl(manifest, pair.remote);
    const remote = Buffer.from(await (await fetchOk(fetchFn, url)).arrayBuffer());
    let local: Buffer | null = null;

    try {
      local = readFileSync(path.join(root, pair.local));
    } catch {
      errors.push(`${pair.local}: missing from the resource directory`);
    }

    if (local && !local.equals(remote)) {
      errors.push(`${pair.local}: differs from upstream ${url}`);
    }
  }

  if (sha256(readFileSync(path.join(root, manifest.license.path))) !== manifest.license.sha256) {
    errors.push(`${manifest.license.path}: SHA-256 differs from manifest`);
  }

  return errors;
}

export async function verifyUpstream(options: {
  fetch: Fetch;
  root: string;
  manifest: Manifest;
  tmpRoot: string;
}): Promise<string[]> {
  const { fetch: fetchFn, root, manifest, tmpRoot } = options;
  const archive = await downloadBottle(
    fetchFn,
    manifest.formula.repository,
    manifest.formula.blobDigest,
  );
  const bottleErrors = withVerifiedBottle(archive, manifest.formula.blobDigest, tmpRoot, (out) =>
    compareWithBottle(out, root, manifest),
  );

  return [...bottleErrors, ...(await compareWithUpstream(fetchFn, root, manifest))];
}

interface FormulaInfo {
  versions: { stable: string };
  urls: { stable: { tag?: string; revision?: string } };
  bottle: { stable: { files: Record<string, { sha256: string }> } };
}

/**
 * Rewrites the resource directory and the manifest from a new bottle. File names and symlinks are
 * taken from the current manifest; the bottle paths follow the new version.
 */
export async function refresh(options: {
  fetch: Fetch;
  root: string;
  manifestPath: string;
  tmpRoot: string;
  bottleTag: string;
  version: string;
}): Promise<void> {
  const { fetch: fetchFn, root, manifestPath, tmpRoot, bottleTag, version } = options;
  const current = readManifest(manifestPath);
  const formulaName = current.formula.name;
  const info = (await (
    await fetchOk(fetchFn, `https://formulae.brew.sh/api/formula/${formulaName}.json`)
  ).json()) as FormulaInfo;

  if (info.versions.stable !== version) {
    throw new Error(`Formula ${formulaName} is at ${info.versions.stable}, not ${version}`);
  }

  const bottle = info.bottle.stable.files[bottleTag];
  const revision = info.urls.stable.revision;
  const tag = info.urls.stable.tag ?? `v${version}`;

  if (!bottle || !revision) {
    throw new Error(`Formula ${formulaName} has no ${bottleTag} bottle or no source revision`);
  }

  const submodule = (await (
    await fetchOk(
      fetchFn,
      `https://api.github.com/repos/${current.upstream.mediaControl.repository}/contents/mediaremote-adapter?ref=${revision}`,
    )
  ).json()) as { sha: string };
  const digest = `sha256:${bottle.sha256}`;
  const oldPrefix = `${formulaName}/${current.formula.version}/`;
  const newPrefix = `${formulaName}/${version}/`;
  const next: Manifest = {
    ...current,
    formula: { ...current.formula, version, bottleTag, blobDigest: digest },
    upstream: {
      mediaControl: { ...current.upstream.mediaControl, tag, revision },
      mediaremoteAdapter: { ...current.upstream.mediaremoteAdapter, tag, revision: submodule.sha },
    },
    files: current.files.map((file) => ({
      ...file,
      bottlePath: file.bottlePath.replace(oldPrefix, newPrefix),
    })),
    symlinks: current.symlinks.map((link) => ({
      ...link,
      bottlePath: link.bottlePath.replace(oldPrefix, newPrefix),
    })),
  };
  const archive = await downloadBottle(fetchFn, next.formula.repository, digest);

  withVerifiedBottle(archive, digest, tmpRoot, (extracted) => {
    for (const file of next.files) {
      const source = path.join(extracted, file.bottlePath);
      const bytes = readFileSync(source);
      const target = path.join(root, file.path);
      const executable = isExecutable(lstatSync(source).mode);

      mkdirSync(path.dirname(target), { recursive: true });
      writeFileSync(target, bytes);
      chmodSync(target, executable ? 0o755 : 0o644);
      file.sha256 = sha256(bytes);
      file.mode = executable ? '100755' : '100644';
    }

    for (const link of next.symlinks) {
      const target = path.join(root, link.path);

      link.target = readlinkSync(path.join(extracted, link.bottlePath));
      rmSync(target, { force: true });
      symlinkSync(link.target, target);
    }
  });

  const license = Buffer.from(
    await (await fetchOk(fetchFn, upstreamUrl(next, next.license.upstreamPath))).arrayBuffer(),
  );

  writeFileSync(path.join(root, next.license.path), license);
  next.license.sha256 = sha256(license);
  writeFileSync(manifestPath, `${JSON.stringify(next, null, 2)}\n`);
}

function argument(args: string[], name: string): string {
  const index = args.indexOf(name);
  const value = index === -1 ? undefined : args[index + 1];

  if (!value || value.startsWith('--')) {
    throw new Error(`${name} <value> is required`);
  }

  return value;
}

function report(label: string, errors: string[]): number {
  if (errors.length > 0) {
    console.error(`${label} failed:\n${errors.map((error) => `  - ${error}`).join('\n')}`);

    return 1;
  }

  console.log(`${label} passed.`);

  return 0;
}

async function main(args: string[]): Promise<number> {
  const manifest = readManifest();

  if (args.includes('--verify-upstream')) {
    return report(
      'media-control --verify-upstream',
      await verifyUpstream({ fetch, root: RESOURCE_DIR, manifest, tmpRoot: os.tmpdir() }),
    );
  }

  if (args.includes('--refresh')) {
    await refresh({
      fetch,
      root: RESOURCE_DIR,
      manifestPath: MANIFEST_PATH,
      tmpRoot: os.tmpdir(),
      bottleTag: argument(args, '--bottle'),
      version: argument(args, '--version'),
    });
    console.log('Refreshed. Run --verify-upstream and the live probe before committing.');

    return report('media-control --check', checkDirectory(RESOURCE_DIR, readManifest()));
  }

  return report('media-control --check', checkDirectory(RESOURCE_DIR, manifest));
}

if (import.meta.main) {
  process.exitCode = await main(process.argv.slice(2));
}
