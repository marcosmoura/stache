import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  verifyReference,
  type ReferenceManifest,
} from '../../src/ui/tests/bar-reference/verify.ts';

import { flattenCss, serializeRules } from './css.ts';

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
