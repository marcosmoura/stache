import { afterEach, describe, expect, it } from 'vitest';

import { fixtures, harnessSources, manifest, referenceFiles } from './reference.ts';
import {
  LAPTOP_MEDIA_QUERY,
  ROOT_FONT_STACK,
  mountReference,
  normalizeFontStack,
  type MountedReference,
} from './render.ts';
import { findForbiddenReferences, verifyReference } from './verify.ts';

let mounted: MountedReference | null = null;

afterEach(() => {
  mounted?.unmount();
  mounted = null;
});

describe('Bar legacy reference provenance', () => {
  it('matches the manifest hashes, pinned objects and icon package integrity', async () => {
    const result = await verifyReference(referenceFiles, manifest, { harnessSources });

    expect(result.problems).toEqual([]);
    expect(result.ok).toBe(true);
  });

  it('detects a tampered reference file', async () => {
    const tampered = {
      ...referenceFiles,
      'reference.css': `${referenceFiles['reference.css']}\n/* x */`,
    };

    const result = await verifyReference(tampered, manifest);

    expect(result.ok).toBe(false);
    expect(result.problems).toContain('reference file hash mismatch: reference.css');
  });

  it('detects references to the new implementation', () => {
    expect(
      findForbiddenReferences('x.ts', "import { Bar } from '@/shell/windows/Bar'"),
    ).not.toEqual([]);
    expect(findForbiddenReferences('x.css', '@import "../../src/shell/Bar.css"')).not.toEqual([]);
    expect(findForbiddenReferences('x.css', '.a { color: red; }')).toEqual([]);
    expect(findForbiddenReferences('x.ts', "import a from '../../src/shell/Bar'")).not.toEqual([]);
  });
});

describe('Bar legacy reference render', () => {
  it('mounts a fixture with the legacy root font stack', async () => {
    mounted = await mountReference({ fixture: 'bar-default', width: 1440 });

    const root = mounted.document.documentElement;
    const style = mounted.frame.contentWindow?.getComputedStyle(root);

    expect(normalizeFontStack(style?.fontFamily ?? '')).toBe(ROOT_FONT_STACK);
    expect(style?.fontSize).toBe('13px');
    expect(mounted.root.querySelector('[data-testid="status-container"]')).not.toBeNull();
    expect(Object.keys(fixtures)).toContain('bar-default');
  });

  it('selects the compact markup from the iframe viewport at the 2036px breakpoint', async () => {
    mounted = await mountReference({ fixture: 'bar-default', width: 2036 });
    expect(mounted.frame.contentWindow?.matchMedia(LAPTOP_MEDIA_QUERY).matches).toBe(true);
    expect(mounted.variant).toBe('compact');
    mounted.unmount();

    mounted = await mountReference({ fixture: 'bar-default', width: 2037 });
    expect(mounted.variant).toBe('wide');
  });
});
