import {
  chmodSync,
  cpSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { gunzipSync, gzipSync } from 'node:zlib';

import { afterEach, describe, expect, it } from 'vitest';

import {
  checkDirectory,
  type Fetch,
  listTarGz,
  type Manifest,
  REPO_ROOT,
  RESOURCE_DIR,
  readManifest,
  refresh,
  sha256,
  thinArm64Problem,
  verifyUpstream,
  withVerifiedBottle,
} from './vendor';

const roots: string[] = [];

function tempDir(prefix: string): string {
  const dir = mkdtempSync(path.join(os.tmpdir(), prefix));

  roots.push(dir);

  return dir;
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

/** A copy of the real resource directory that the negative cases can modify. */
function copyResources(): string {
  const root = path.join(tempDir('media-control-check-'), 'media-control');

  cpSync(RESOURCE_DIR, root, { recursive: true, verbatimSymlinks: true });

  return root;
}

function expectNamed(errors: string[], name: string): void {
  expect(errors.length).toBeGreaterThan(0);
  expect(errors.some((error) => error.startsWith(`${name}:`))).toBe(true);
}

describe('media-control --check', () => {
  const manifest = readManifest();

  it('passes on the real directory against the real manifest', () => {
    expect(checkDirectory(RESOURCE_DIR, manifest)).toEqual([]);
  });

  it('records both Mach-O files as thin arm64', () => {
    const machO = manifest.files.filter((file) => file.machO);

    expect(machO).toHaveLength(2);

    for (const file of machO) {
      expect(thinArm64Problem(readFileSync(path.join(RESOURCE_DIR, file.path)))).toBeNull();
    }
  });

  it('names a file with one changed byte', () => {
    const root = copyResources();
    const file = path.join(root, 'mediaremote-adapter.pl');
    const bytes = readFileSync(file);

    bytes[10] ^= 1;
    writeFileSync(file, bytes);

    expectNamed(checkDirectory(root, manifest), 'mediaremote-adapter.pl');
  });

  it('names a file that lost its executable bit', () => {
    const root = copyResources();

    chmodSync(path.join(root, 'MediaRemoteAdapterTestClient'), 0o644);

    expectNamed(checkDirectory(root, manifest), 'MediaRemoteAdapterTestClient');
  });

  it('ignores umask differences that git does not preserve', () => {
    const root = copyResources();

    chmodSync(path.join(root, 'MediaRemoteAdapterTestClient'), 0o700);
    chmodSync(path.join(root, 'LICENSE'), 0o600);

    expect(checkDirectory(root, manifest)).toEqual([]);
  });

  it('names a missing or retargeted symlink', () => {
    const missing = copyResources();

    rmSync(path.join(missing, 'MediaRemoteAdapter.framework/Versions/Current'));
    expectNamed(checkDirectory(missing, manifest), 'MediaRemoteAdapter.framework/Versions/Current');

    const retargeted = copyResources();
    const link = path.join(retargeted, 'MediaRemoteAdapter.framework/Resources');

    rmSync(link);
    symlinkSync('Versions/A/Resources', link);
    expectNamed(checkDirectory(retargeted, manifest), 'MediaRemoteAdapter.framework/Resources');
  });

  it('names an extra entry and says it may be a tool artifact', () => {
    const root = copyResources();

    mkdirSync(path.join(root, '.claude'));

    const errors = checkDirectory(root, manifest);

    expectNamed(errors, '.claude');
    expect(errors.join('\n')).toMatch(/unexpected entry.*tool artifact/);
  });

  it('names non-arm64 and fat Mach-O headers', () => {
    const header = (magic: string, cpu: number) => {
      const bytes = Buffer.alloc(32);

      Buffer.from(magic, 'hex').copy(bytes);
      bytes.writeUInt32LE(cpu, 4);

      return bytes;
    };
    const x86 = header('cffaedfe', 0x01000007);
    const fat = header('cafebabe', 2);
    const files = [
      { path: 'thin-x86', bytes: x86 },
      { path: 'fat', bytes: fat },
    ];
    const root = copyResources();
    const synthetic: Manifest = {
      ...manifest,
      files: [
        ...manifest.files,
        ...files.map((file) => ({
          path: file.path,
          bottlePath: file.path,
          sha256: sha256(file.bytes),
          mode: '100755',
          machO: true,
        })),
      ],
    };

    for (const file of files) {
      writeFileSync(path.join(root, file.path), file.bytes, { mode: 0o755 });
    }

    const errors = checkDirectory(root, synthetic);

    expect(errors.find((error) => error.startsWith('thin-x86:'))).toMatch(/not arm64/);
    expect(errors.find((error) => error.startsWith('fat:'))).toMatch(/fat/);
  });

  it('names a manifest entry without a file, and a file without a manifest entry', () => {
    const root = copyResources();
    const fewer: Manifest = { ...manifest, files: manifest.files.slice(1) };
    const more: Manifest = {
      ...manifest,
      files: [...manifest.files, { ...manifest.files[0], path: 'absent.pl' }],
    };

    expectNamed(checkDirectory(root, fewer), manifest.files[0].path);
    expectNamed(checkDirectory(root, more), 'absent.pl');
  });
});

interface TarInput {
  name: string;
  type?: '0' | '2' | '5';
  data?: Buffer;
  linkName?: string;
  mode?: number;
}

function octal(value: number, length: number): string {
  return `${value.toString(8).padStart(length - 1, '0')}\0`;
}

/** Builds a gzipped ustar archive in memory (no system tar, so unsafe entries can be written). */
function tarGz(entries: TarInput[]): Buffer {
  const blocks: Buffer[] = [];

  for (const entry of entries) {
    const data = entry.data ?? Buffer.alloc(0);
    const type = entry.type ?? '0';
    const header = Buffer.alloc(512);

    header.write(entry.name, 0, 100, 'utf8');
    header.write(octal(entry.mode ?? (type === '0' ? 0o644 : 0o755), 8), 100);
    header.write(octal(0, 8), 108);
    header.write(octal(0, 8), 116);
    header.write(octal(type === '0' ? data.length : 0, 12), 124);
    header.write(octal(0, 12), 136);
    header.write('        ', 148);
    header.write(type, 156);
    header.write(entry.linkName ?? '', 157, 100, 'utf8');
    header.write('ustar\0', 257);
    header.write('00', 263);

    const checksum = header.reduce((sum, byte) => sum + byte, 0);

    header.write(`${checksum.toString(8).padStart(6, '0')}\0 `, 148);
    blocks.push(header);

    if (type === '0') {
      blocks.push(data, Buffer.alloc((512 - (data.length % 512)) % 512));
    }
  }

  blocks.push(Buffer.alloc(1024));

  return gzipSync(Buffer.concat(blocks));
}

/** A bottle built from the committed resource files at the manifest's bottle paths. */
function bottleEntries(manifest: Manifest): TarInput[] {
  const directories = new Set<string>();
  const entries: TarInput[] = [];

  for (const entry of [...manifest.files, ...manifest.symlinks]) {
    for (
      let dir = path.posix.dirname(entry.bottlePath);
      dir !== '.';
      dir = path.posix.dirname(dir)
    ) {
      directories.add(dir);
    }
  }

  for (const dir of [...directories].sort()) {
    entries.push({ name: `${dir}/`, type: '5' });
  }

  for (const file of manifest.files) {
    entries.push({
      name: file.bottlePath,
      data: readFileSync(path.join(RESOURCE_DIR, file.path)),
      mode: file.mode === '100755' ? 0o555 : 0o644,
    });
  }

  for (const link of manifest.symlinks) {
    entries.push({ name: link.bottlePath, type: '2', linkName: link.target });
  }

  return entries;
}

function fakeFetch(
  archive: Buffer,
  upstream: Record<string, Buffer>,
): { fetch: Fetch; urls: string[] } {
  const urls: string[] = [];
  const fetch: Fetch = async (url) => {
    urls.push(url);

    if (url.startsWith('https://ghcr.io/token')) {
      return Response.json({ token: 'anonymous' });
    }

    if (url.startsWith('https://ghcr.io/v2/')) {
      return new Response(new Uint8Array(archive));
    }

    const file = Object.keys(upstream).find((name) => url.endsWith(`/${name}`));

    if (file) {
      return new Response(new Uint8Array(upstream[file]));
    }

    return new Response('not found', { status: 404 });
  };

  return { fetch, urls };
}

describe('media-control --verify-upstream (synthetic bottle, no network)', () => {
  const real = readManifest();
  const upstreamFiles = (): Record<string, Buffer> => ({
    'bin/mediaremote-adapter.pl': readFileSync(path.join(RESOURCE_DIR, 'mediaremote-adapter.pl')),
    LICENSE: readFileSync(path.join(RESOURCE_DIR, 'LICENSE')),
  });

  function run(entries: TarInput[], upstream = upstreamFiles(), root = RESOURCE_DIR) {
    const archive = tarGz(entries);
    const manifest: Manifest = {
      ...real,
      formula: { ...real.formula, blobDigest: `sha256:${sha256(archive)}` },
    };
    const tmpRoot = tempDir('media-control-verify-');
    const { fetch, urls } = fakeFetch(archive, upstream);

    return {
      tmpRoot,
      urls,
      result: verifyUpstream({ fetch, root, manifest, tmpRoot }),
    };
  }

  it('passes on a bottle that matches, pins the revision and cleans up', async () => {
    const { result, urls, tmpRoot } = run(bottleEntries(real));

    await expect(result).resolves.toEqual([]);
    expect(urls.some((url) => url.includes(real.upstream.mediaremoteAdapter.revision))).toBe(true);
    expect(urls.some((url) => url.includes('/v0.7.7/'))).toBe(false);
    expect(readdirSync(tmpRoot)).toEqual([]);
  });

  it('refuses a digest mismatch before extracting anything', async () => {
    const archive = tarGz(bottleEntries(real));
    const tmpRoot = tempDir('media-control-verify-');
    const { fetch } = fakeFetch(archive, upstreamFiles());

    await expect(
      verifyUpstream({ fetch, root: RESOURCE_DIR, manifest: real, tmpRoot }),
    ).rejects.toThrow(/digest .* differs from manifest/);
    expect(readdirSync(tmpRoot)).toEqual([]);
  });

  it.each([
    ['a ../ entry', { name: 'media-control/../escape', data: Buffer.from('x') }],
    ['an absolute entry', { name: '/tmp/escape', data: Buffer.from('x') }],
    [
      'a symlink pointing outside',
      { name: 'media-control/0.7.7/out', type: '2', linkName: '../../../etc' },
    ],
  ] as const)('refuses %s before extraction', async (_label, bad) => {
    const { result, tmpRoot } = run([...bottleEntries(real), bad as TarInput]);

    await expect(result).rejects.toThrow(/Unsafe bottle archive/);
    expect(readdirSync(tmpRoot)).toEqual([]);
  });

  it('names a committed file that differs from the bottle', async () => {
    const entries = bottleEntries(real);
    const target = entries.find((entry) => entry.name === real.files[3].bottlePath);

    target!.data = Buffer.from('changed');

    const errors = await run(entries).result;

    expectNamed(errors, real.files[3].path);
  });

  it('names a script that differs from upstream', async () => {
    const upstream = upstreamFiles();

    upstream['bin/mediaremote-adapter.pl'] = Buffer.from('#!/usr/bin/perl\n');

    const errors = await run(bottleEntries(real), upstream).result;

    expectNamed(errors, 'mediaremote-adapter.pl');
  });

  it('lists symlink entries with their targets', () => {
    const listing = listTarGz(tarGz(bottleEntries(real)));

    expect(listing.filter((entry) => entry.type === '2').map((entry) => entry.linkName)).toEqual(
      real.symlinks.map((link) => link.target),
    );
  });

  it('refuses an archive whose entry size cannot be read, before extraction', () => {
    const tar = gunzipSync(tarGz(bottleEntries(real)));

    // A base-256 size (high bit set) is not octal text; the lister must not stop silently there.
    tar[124] = 0x80;

    const archive = gzipSync(tar);
    const tmpRoot = tempDir('media-control-verify-');

    expect(() =>
      withVerifiedBottle(archive, `sha256:${sha256(archive)}`, tmpRoot, () => null),
    ).toThrow(/Malformed tar header/);
    expect(readdirSync(tmpRoot)).toEqual([]);
  });
});

describe('media-control --refresh (synthetic bottle, no network)', () => {
  const real = readManifest();
  const mediaControlRevision = 'a'.repeat(40);
  const adapterRevision = 'b'.repeat(40);
  const license = Buffer.from('New licence text\n');

  /** The current file set at the bottle paths of version 0.7.8, with a changed `Info.plist`. */
  function nextBottle(): Buffer {
    const moved = (bottlePath: string) => bottlePath.replace('/0.7.7/', '/0.7.8/');
    const entries = bottleEntries({
      ...real,
      files: real.files.map((file) => ({ ...file, bottlePath: moved(file.bottlePath) })),
      symlinks: real.symlinks.map((link) => ({ ...link, bottlePath: moved(link.bottlePath) })),
    });
    const plist = entries.find((entry) => entry.name.endsWith('/Info.plist'));

    plist!.data = Buffer.from('<plist>new</plist>\n');

    return tarGz(entries);
  }

  function setup(archive: Buffer, stable: string, tagRevision: string | null = adapterRevision) {
    const work = tempDir('media-control-refresh-');
    const root = path.join(work, 'media-control');
    const manifestPath = path.join(work, 'manifest.json');
    const tmpRoot = path.join(work, 'tmp');
    const fetch: Fetch = async (url) => {
      if (url === 'https://formulae.brew.sh/api/formula/media-control.json') {
        return Response.json({
          versions: { stable },
          urls: { stable: { tag: `v${stable}`, revision: mediaControlRevision } },
          bottle: { stable: { files: { arm64_tahoe: { sha256: sha256(archive) } } } },
        });
      }

      if (url.startsWith('https://api.github.com/repos/ungive/media-control/contents/')) {
        return Response.json({ sha: adapterRevision });
      }

      if (url === `https://api.github.com/repos/ungive/mediaremote-adapter/commits/v${stable}`) {
        return tagRevision
          ? Response.json({ sha: tagRevision })
          : new Response('', { status: 404 });
      }

      if (url.startsWith('https://ghcr.io/token')) {
        return Response.json({ token: 'anonymous' });
      }

      if (url.startsWith('https://ghcr.io/v2/')) {
        return new Response(new Uint8Array(archive));
      }

      if (url.endsWith(`/ungive/mediaremote-adapter/${adapterRevision}/LICENSE`)) {
        return new Response(new Uint8Array(license));
      }

      return new Response('not found', { status: 404 });
    };

    mkdirSync(root);
    mkdirSync(tmpRoot);
    writeFileSync(manifestPath, JSON.stringify(real));

    return { fetch, root, manifestPath, tmpRoot };
  }

  it('rewrites the files, symlinks, licence and manifest from the new bottle', async () => {
    const archive = nextBottle();
    const { fetch, root, manifestPath, tmpRoot } = setup(archive, '0.7.8');

    const notes = await refresh({
      fetch,
      root,
      manifestPath,
      tmpRoot,
      bottleTag: 'arm64_tahoe',
      version: '0.7.8',
    });

    const written = readManifest(manifestPath);
    const plist = written.files.find((file) => file.path.endsWith('/Info.plist'));

    expect(written.formula).toMatchObject({
      version: '0.7.8',
      bottleTag: 'arm64_tahoe',
      blobDigest: `sha256:${sha256(archive)}`,
    });
    expect(written.upstream.mediaControl.revision).toBe(mediaControlRevision);
    expect(written.upstream.mediaremoteAdapter.revision).toBe(adapterRevision);
    expect(written.license.sha256).toBe(sha256(license));
    expect(plist!.bottlePath).toContain('/0.7.8/');
    expect(plist!.sha256).toBe(sha256(Buffer.from('<plist>new</plist>\n')));
    expect(readFileSync(path.join(root, plist!.path), 'utf8')).toBe('<plist>new</plist>\n');
    expect(written.upstream.mediaremoteAdapter.tag).toBe('v0.7.8');
    expect(checkDirectory(root, written)).toEqual([]);
    expect(notes).toEqual([]);
    expect(readdirSync(tmpRoot)).toEqual([]);
  });

  it('notes a bottle file the manifest does not list, without failing', async () => {
    const entries = gunzipSync(nextBottle());
    const extra = tarGz([
      { name: 'media-control/0.7.8/lib/media-control/new-helper', data: Buffer.from('x') },
    ]);
    // Append the extra entry by rebuilding: drop the trailing zero blocks of the first archive.
    const archive = gzipSync(
      Buffer.concat([entries.subarray(0, entries.length - 1024), gunzipSync(extra)]),
    );
    const { fetch, root, manifestPath, tmpRoot } = setup(archive, '0.7.8');
    const notes = await refresh({
      fetch,
      root,
      manifestPath,
      tmpRoot,
      bottleTag: 'arm64_tahoe',
      version: '0.7.8',
    });

    expect(notes).toEqual([
      'media-control/0.7.8/lib/media-control/new-helper: in the bottle but not in the manifest; it was not vendored',
    ]);
    expect(checkDirectory(root, readManifest(manifestPath))).toEqual([]);
  });

  it('writes nothing when the bottle lacks a manifest path', async () => {
    const lacking = bottleEntries({
      ...real,
      files: real.files.map((file) => ({
        ...file,
        bottlePath: file.bottlePath.replace('/0.7.7/', '/0.7.8/'),
      })),
      symlinks: real.symlinks.map((link) => ({
        ...link,
        bottlePath: link.bottlePath.replace('/0.7.7/', '/0.7.8/'),
      })),
    }).filter((entry) => !entry.name.endsWith('/Info.plist'));
    const { fetch, root, manifestPath, tmpRoot } = setup(tarGz(lacking), '0.7.8');

    await expect(
      refresh({ fetch, root, manifestPath, tmpRoot, bottleTag: 'arm64_tahoe', version: '0.7.8' }),
    ).rejects.toThrow(/Info\.plist: missing from the bottle .*nothing written/);
    expect(readdirSync(root)).toEqual([]);
    expect(readManifest(manifestPath)).toEqual(real);
    expect(readdirSync(tmpRoot)).toEqual([]);
  });

  it.each([
    ['points at another commit', 'c'.repeat(40), /is c{40}, not the submodule revision/],
    ['does not exist', null, /does not exist/],
  ])('writes nothing when the adapter tag %s', async (_label, tagRevision, message) => {
    const { fetch, root, manifestPath, tmpRoot } = setup(nextBottle(), '0.7.8', tagRevision);
    const run = refresh({
      fetch,
      root,
      manifestPath,
      tmpRoot,
      bottleTag: 'arm64_tahoe',
      version: '0.7.8',
    });

    await expect(run).rejects.toThrow(message);
    await expect(run).rejects.toThrow(/adapter tag must be set by hand/);
    expect(readdirSync(root)).toEqual([]);
    expect(readManifest(manifestPath)).toEqual(real);
  });

  it('reports a failed tag lookup as it is, not as a missing tag', async () => {
    const { fetch, root, manifestPath, tmpRoot } = setup(nextBottle(), '0.7.8');
    const limited: Fetch = async (url, init) =>
      url.includes('/mediaremote-adapter/commits/')
        ? new Response('rate limited', { status: 403 })
        : fetch(url, init);

    await expect(
      refresh({
        fetch: limited,
        root,
        manifestPath,
        tmpRoot,
        bottleTag: 'arm64_tahoe',
        version: '0.7.8',
      }),
    ).rejects.toThrow(/Download failed \(403\)/);
    expect(readdirSync(root)).toEqual([]);
    expect(readManifest(manifestPath)).toEqual(real);
  });

  it('writes nothing when the formula is at another version', async () => {
    const { fetch, root, manifestPath, tmpRoot } = setup(nextBottle(), '0.7.9');

    await expect(
      refresh({ fetch, root, manifestPath, tmpRoot, bottleTag: 'arm64_tahoe', version: '0.7.8' }),
    ).rejects.toThrow(/is at 0\.7\.9, not 0\.7\.8/);
    expect(readdirSync(root)).toEqual([]);
    expect(readManifest(manifestPath)).toEqual(real);
  });
});

describe('media-control human-readable records', () => {
  const manifest = readManifest();

  it('PROVENANCE.md and bar.md name what the manifest pins (update them by hand after a refresh)', () => {
    const provenance = readFileSync(path.join(RESOURCE_DIR, 'PROVENANCE.md'), 'utf8');
    const bar = readFileSync(path.join(REPO_ROOT, 'docs/features/bar.md'), 'utf8');
    const pinned = [
      manifest.formula.version,
      manifest.formula.bottleTag,
      manifest.formula.blobDigest,
      manifest.upstream.mediaControl.revision,
      manifest.upstream.mediaremoteAdapter.revision,
      ...manifest.files.map((file) => file.sha256),
    ];

    for (const value of pinned) {
      expect(provenance, `PROVENANCE.md lacks ${value}`).toContain(value);
    }

    expect(bar).toContain(
      `media-control ${manifest.formula.version}\` \`${manifest.formula.bottleTag}\``,
    );
  });
});
