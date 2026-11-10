/**
 * Pure provenance verifier for the committed Bar legacy reference. No network, no file system:
 * callers pass the file contents, so it runs in WebKit (Vitest browser) and in Node/Bun alike.
 *
 * It recomputes SHA-256 of every reference output, checks them against `manifest.json`, checks the
 * pinned commit/object IDs and legacy icon package integrity against independent constants copied
 * from the spec, and fails if reference files point at the new implementation.
 */

/** Pinned commit from `docs/specs/2026-11-03-bar/04-ui-shell-parity.md`. */
export const PINNED_COMMIT = 'e0b4648ed22891d3ba29abf3d8fa8f56f949c657';

/** Git object table from the same spec (paths relative to the repository root). */
export const PINNED_OBJECTS: Readonly<Record<string, string>> = {
  '_legacy/app/ui/renderer/bar': '57d4212a9f591d3230076918a2f6527b7baf5a67',
  '_legacy/app/ui/stores': '2dc990c01a99673cdce2260f577896283f1d909a',
  '_legacy/app/ui/design-system': 'b8af6c5b4cdd411d5a4c752b89c18dcff772d050',
  '_legacy/app/ui/components': 'f935047c48d6cb595f12f0ea24a98327bb41bc55',
  '_legacy/app/ui/main.css': 'd9080d2880fc419ada4133530a2eec31ec4d18b8',
  '_legacy/app/native/src/modules/bar': '093dc10ac555e036f70eb71c520653bcea6f9539',
};

/** Legacy icon packages from `_legacy/pnpm-lock.yaml` at the pinned commit. */
export const LEGACY_ICON_PACKAGES: Readonly<
  Record<string, { version: string; integrity: string }>
> = {
  '@hugeicons/core-free-icons': {
    version: '4.2.3',
    integrity:
      'sha512-fpiU0qFZkv4ABi23xJ2m4qNr0BBfuIaYjPboN8WDA6aj9D+OzXa7eykxKOpACZRuGbDz1U9S5DsNx1d5KNnsgA==',
  },
  '@hugeicons/react': {
    version: '1.1.9',
    integrity:
      'sha512-O+lWSWjbijoAvMCxn4K2bQWCGN5+mP1y5j+X99j23mXMj+s0X25fs71T6t9YJLaBodwmZdaewD27dzS/PiboQw==',
  },
  '@icons-pack/react-simple-icons': {
    version: '13.15.1',
    integrity:
      'sha512-S35E7vJlJzMeoqjwwI7ZmQUJ2W+2KspnB6bc49KdoDGwp5mCgAy9L3sscfWMewQSMgBdPU8KWWwpSGq5DJyAFw==',
  },
};

export interface ReferenceManifest {
  schema: number;
  pinnedCommit: string;
  pinnedObjects: { path: string; expected: string; actual: string }[];
  sources: { path: string; blob: string; sha256: string; pinnedBy: string }[];
  lockfile: { path: string; blob: string; sha256: string };
  iconPackages: { name: string; version: string; integrity: string; license: string }[];
  icons: { file: string; sha256: string; sourceSha256: string | null; package: string }[];
  outputs: { path: string; sha256: string }[];
  extractionTool: { files: { path: string; sha256: string }[]; sha256: string };
  handReconstructed: { piece: string; detail: string }[];
}

export interface VerifyOptions {
  /** Tool sources (`scripts/bar-reference/*.ts`) keyed by repository path, when available. */
  extractionTool?: Readonly<Record<string, string>>;
  /** Harness sources scanned for imports of the new implementation. */
  harnessSources?: Readonly<Record<string, string>>;
}

export interface VerifyResult {
  ok: boolean;
  problems: string[];
}

export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));

  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

// Built from fragments so this verifier never matches its own scan.
const NEW_SRC = ['src', 'ui', 'src'].join('/');
const FORBIDDEN: readonly { label: string; pattern: RegExp }[] = [
  { label: `path ${NEW_SRC}`, pattern: new RegExp(NEW_SRC.replaceAll('/', '\\/')) },
  { label: 'shell window path', pattern: /shell\/(windows|components)\//u },
  { label: 'alias import @/', pattern: /(?:from|import)\s*\(?\s*['"]@\//u },
  { label: 'relative import into src', pattern: /(?:from|import)\s*\(?\s*['"](?:\.\.\/)+src\//u },
];

export function findForbiddenReferences(name: string, content: string): string[] {
  return FORBIDDEN.filter(({ pattern }) => pattern.test(content)).map(
    ({ label }) => `${name} references the new implementation (${label})`,
  );
}

export async function verifyReference(
  files: Readonly<Record<string, string>>,
  manifest: ReferenceManifest,
  options: VerifyOptions = {},
): Promise<VerifyResult> {
  const problems: string[] = [];

  if (manifest.pinnedCommit !== PINNED_COMMIT) {
    problems.push(`pinned commit ${manifest.pinnedCommit} != ${PINNED_COMMIT}`);
  }

  const verified = new Map(manifest.pinnedObjects.map((entry) => [entry.path, entry]));

  for (const [objectPath, object] of Object.entries(PINNED_OBJECTS)) {
    const entry = verified.get(objectPath);

    if (!entry || entry.expected !== object || entry.actual !== object) {
      problems.push(`pinned object ${objectPath} is not verified as ${object}`);
    }
  }

  if (verified.size !== Object.keys(PINNED_OBJECTS).length) {
    problems.push('manifest pinnedObjects has unexpected entries');
  }

  for (const [name, expected] of Object.entries(LEGACY_ICON_PACKAGES)) {
    const record = manifest.iconPackages.find((entry) => entry.name === name);

    if (!record || record.version !== expected.version || record.integrity !== expected.integrity) {
      problems.push(
        `icon package ${name}@${expected.version} integrity does not match the legacy lockfile`,
      );
    }

    if (record && !record.license) {
      problems.push(`icon package ${name} has no recorded license`);
    }
  }

  if (manifest.iconPackages.length !== Object.keys(LEGACY_ICON_PACKAGES).length) {
    problems.push('manifest iconPackages has unexpected entries');
  }

  if (
    !/^[0-9a-f]{40}$/u.test(manifest.lockfile.blob) ||
    !/^[0-9a-f]{64}$/u.test(manifest.lockfile.sha256)
  ) {
    problems.push('lockfile provenance is malformed');
  }

  for (const source of manifest.sources) {
    if (!/^[0-9a-f]{40}$/u.test(source.blob) || !/^[0-9a-f]{64}$/u.test(source.sha256)) {
      problems.push(`source ${source.path} provenance is malformed`);
    }
  }

  const listed = new Set(manifest.outputs.map((output) => output.path));

  for (const output of manifest.outputs) {
    const content = files[output.path];

    if (content === undefined) {
      problems.push(`reference file missing: ${output.path}`);
    } else if ((await sha256Hex(content)) !== output.sha256) {
      problems.push(`reference file hash mismatch: ${output.path}`);
    }
  }

  for (const name of Object.keys(files)) {
    if (!listed.has(name)) {
      problems.push(`reference file not in manifest: ${name}`);
    }
  }

  for (const icon of manifest.icons) {
    const output = manifest.outputs.find((entry) => entry.path === icon.file);

    if (!output || output.sha256 !== icon.sha256) {
      problems.push(`icon ${icon.file} hash is not the manifest output hash`);
    }

    if (!icon.sourceSha256) {
      problems.push(`icon ${icon.file} has no source SVG data hash`);
    }
  }

  for (const [name, content] of Object.entries(files)) {
    problems.push(...findForbiddenReferences(name, content));
  }

  for (const [name, content] of Object.entries(options.harnessSources ?? {})) {
    problems.push(...findForbiddenReferences(name, content));
  }

  if (options.extractionTool) {
    for (const file of manifest.extractionTool.files) {
      const content = options.extractionTool[file.path];

      if (content === undefined || (await sha256Hex(content)) !== file.sha256) {
        problems.push(`extraction tool changed since extraction: ${file.path}`);
      }
    }
  }

  if (manifest.handReconstructed.length === 0) {
    problems.push('manifest must list hand-reconstructed pieces');
  }

  return { ok: problems.length === 0, problems };
}
