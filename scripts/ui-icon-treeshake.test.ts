import path from 'node:path';

import { build, type Rolldown } from 'vite';
import { describe, expect, it } from 'vitest';

/**
 * Bundles a probe that imports one icon by name, the way call sites do, with
 * the production minifier, and proves no other icon reaches the output.
 */
const ROOT = path.resolve(import.meta.dirname, '..');
/** A virtual module at the repository root, so the bare import resolves; never written. */
const ENTRY = path.join(ROOT, 'src/ui/src/__icon_probe__.ts');
const PROBE =
  "import { Home01Icon } from '@hugeicons/core-free-icons';\n\nexport const icons = [Home01Icon];\n";

describe('Hugeicons tree shaking (node)', () => {
  it('keeps only the icons a call site imports', async () => {
    const output = (await build({
      configFile: false,
      logLevel: 'silent',
      root: ROOT,
      plugins: [
        {
          name: 'icon-probe',
          resolveId: (id) => (id === ENTRY ? ENTRY : null),
          load: (id) => (id === ENTRY ? PROBE : null),
        },
      ],
      build: {
        write: false,
        minify: true,
        lib: { entry: ENTRY, formats: ['es'], fileName: 'probe' },
        rollupOptions: { external: [] },
      },
    })) as Rolldown.RolldownOutput | Rolldown.RolldownOutput[];
    const outputs = Array.isArray(output) ? output : [output];
    const code = outputs
      .flatMap((result) => result.output)
      .map((chunk) => (chunk.type === 'chunk' ? chunk.code : ''))
      .join('\n');
    const svgElements = code.match(/\["(path|circle|rect|line|polyline|polygon|ellipse)"/g) ?? [];

    // Home01Icon is a handful of SVG elements; the full set has thousands.
    expect(svgElements.length).toBeGreaterThan(0);
    expect(svgElements.length).toBeLessThan(10);
    expect(code.length).toBeLessThan(4_000);
  });
});
