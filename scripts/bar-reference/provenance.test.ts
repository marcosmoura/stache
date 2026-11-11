import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  verifyReference,
  type ReferenceManifest,
} from '../../src/ui/tests/bar-reference/verify.ts';

import { flattenCss, serializeRules } from './css.ts';
import { bunLockIntegrity, STRIP_HUGEICONS_PACKAGE } from './npm.ts';

const ROOT = fileURLToPath(new URL('../..', import.meta.url));
const REFERENCE = path.join(ROOT, 'src/ui/tests/bar-reference');

function readTree(directory: string, prefix = ''): Record<string, string> {
  const files: Record<string, string> = {};

  for (const entry of readdirSync(path.join(directory, prefix), { withFileTypes: true })) {
    const relative = path.posix.join(prefix, entry.name);

    if (entry.isDirectory()) {
      Object.assign(files, readTree(directory, relative));
    } else {
      files[relative] = readFileSync(path.join(directory, relative), 'utf8');
    }
  }

  return files;
}

describe('bar-reference provenance (node)', () => {
  it('verifies committed reference files and the extraction tool against the manifest', async () => {
    const manifest = JSON.parse(
      readFileSync(path.join(REFERENCE, 'manifest.json'), 'utf8'),
    ) as ReferenceManifest;
    const files = Object.fromEntries(
      Object.entries(readTree(REFERENCE)).filter(
        ([name]) =>
          name === 'reference.css' || name.startsWith('fixtures/') || name.startsWith('icons/'),
      ),
    );
    const extractionTool = Object.fromEntries(
      manifest.extractionTool.files.map((file) => [
        file.path,
        readFileSync(path.join(ROOT, file.path), 'utf8'),
      ]),
    );

    const result = await verifyReference(files, manifest, { extractionTool });

    expect(result.problems).toEqual([]);
  });
});

interface StripManifest {
  sources: { path: string; blob: string; sha256: string; pinnedBy: string }[];
  outputs: { path: string; sha256: string }[];
  scenarios: string[];
  stripIcons: {
    provenance: string;
    package: { name: string; version: string; integrity: string; integritySource: string };
    appMapAdditions: { provenance: string; entries: Record<string, string> };
    icons: { file: string; sha256: string; sourceSha256: string | null; packageVersion: string }[];
  };
}

/** Tiling strip provenance checks the shared verifier does not know about (2026-11-10 decisions). */
function verifyStrip(manifest: StripManifest, bunLock: string): string[] {
  const problems: string[] = [];
  const strip = manifest.stripIcons;
  const expected = bunLockIntegrity(bunLock, STRIP_HUGEICONS_PACKAGE);

  if (
    strip.package.name !== STRIP_HUGEICONS_PACKAGE.name ||
    strip.package.version !== STRIP_HUGEICONS_PACKAGE.version ||
    strip.package.integrity !== expected
  ) {
    problems.push('strip hugeicons package does not match the repository bun.lock');
  }

  if (strip.provenance !== 'user decision 2026-11-10') {
    problems.push('strip icon provenance is not the recorded user decision');
  }

  if (strip.appMapAdditions.provenance !== 'user decision 2026-11-10') {
    problems.push('app-map additions provenance is not the recorded user decision');
  }

  for (const icon of strip.icons) {
    const output = manifest.outputs.find((entry) => entry.path === icon.file);

    if (!output || output.sha256 !== icon.sha256 || !icon.sourceSha256) {
      problems.push(`strip icon ${icon.file} is not backed by a manifest output and source hash`);
    }
  }

  const required = [
    'Spaces.constants.ts',
    'Spaces.state.ts',
    'Spaces.styles.ts',
    'Spaces.tsx',
    'components/App/App.tsx',
    'components/App/App.styles.ts',
    'components/AppList/AppList.tsx',
    'components/Workspace/Workspace.tsx',
    'components/Workspace/Workspace.styles.ts',
    'components/WorkspaceList/WorkspaceList.tsx',
    'components/WorkspaceList/WorkspaceList.styles.ts',
  ];

  for (const file of required) {
    const source = manifest.sources.find(
      (entry) => entry.path === `_legacy/app/ui/renderer/bar/Spaces/${file}`,
    );

    if (source?.pinnedBy !== 'table:ui/renderer/bar') {
      problems.push(`strip source ${file} is not pinned by the renderer/bar tree`);
    }
  }

  return problems;
}

describe('bar-reference strip provenance (node)', () => {
  const manifest = JSON.parse(
    readFileSync(path.join(REFERENCE, 'manifest.json'), 'utf8'),
  ) as StripManifest;
  const bunLock = readFileSync(path.join(ROOT, 'bun.lock'), 'utf8');

  it('pins the strip sources and the 4.4.0 strip icons to the repository lockfile', () => {
    expect(verifyStrip(manifest, bunLock)).toEqual([]);
    expect(manifest.stripIcons.icons.length).toBeGreaterThan(0);
    expect(manifest.scenarios.filter((name) => name.startsWith('spaces-')).length).toBe(25);
  });

  it('fails when the strip package integrity, a strip icon hash or the decision is tampered', () => {
    const tamper = (change: (copy: StripManifest) => void) => {
      const copy = structuredClone(manifest);

      change(copy);

      return verifyStrip(copy, bunLock);
    };

    expect(
      tamper((copy) => {
        copy.stripIcons.package.integrity = 'sha512-AAAA';
      }),
    ).not.toEqual([]);
    expect(
      tamper((copy) => {
        const icon = copy.stripIcons.icons[0];

        if (icon) {
          icon.sha256 = '0'.repeat(64);
        }
      }),
    ).not.toEqual([]);
    expect(
      tamper((copy) => {
        copy.stripIcons.provenance = 'unknown';
      }),
    ).not.toEqual([]);
    expect(
      tamper((copy) => {
        copy.sources = copy.sources.filter((entry) => !entry.path.endsWith('App/App.tsx'));
      }),
    ).not.toEqual([]);
  });
});

describe('flattenCss', () => {
  it('flattens nesting, media queries and hoists keyframes', () => {
    const flat = flattenCss(
      'x',
      `
      color: red; /* comment */
      &:hover { color: blue; }
      &::before, &::after { content: '\u00A0'; }
      @media (width <= 2036px) { max-width: 1px; }
      @keyframes spin { 0%, 10% { opacity: 0; } 100% { opacity: 1; } }
    `,
    );

    expect(serializeRules(flat)).toBe(
      [
        '.x {\n  color: red;\n}',
        '.x:hover {\n  color: blue;\n}',
        ".x::before,\n.x::after {\n  content: '\\0000a0';\n}",
        '@media (width <= 2036px) {\n  .x {\n    max-width: 1px;\n  }\n}',
        '@keyframes spin {\n  0%, 10% {\n    opacity: 0;\n  }\n  100% {\n    opacity: 1;\n  }\n}',
      ].join('\n\n'),
    );
  });

  it('fails on unsupported at-rules instead of guessing', () => {
    expect(() => flattenCss('x', '@supports (a: b) { color: red; }')).toThrow(/Unsupported/u);
  });
});
