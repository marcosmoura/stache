import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { afterAll, beforeAll, describe, expect, it } from 'vitest';

/**
 * Runs the repository `.oxlintrc.json` against probe files laid out like the UI
 * tree, in a temporary directory, so the source tree is never touched.
 */
const ROOT = path.resolve(import.meta.dirname, '..');
const OXLINT = path.join(ROOT, 'node_modules/.bin/oxlint');

let workspace = '';

const probe = (relative: string, source: string) => {
  const file = path.join(workspace, relative);

  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, source);

  const result = spawnSync(OXLINT, ['--deny-warnings', '--format', 'unix', relative], {
    cwd: workspace,
    encoding: 'utf8',
  });

  return { status: result.status, output: `${result.stdout}${result.stderr}` };
};

beforeAll(() => {
  workspace = mkdtempSync(path.join(os.tmpdir(), 'stache-ui-lint-'));
  copyFileSync(path.join(ROOT, '.oxlintrc.json'), path.join(workspace, '.oxlintrc.json'));
  // The JS plugins resolve relative to the config file.
  symlinkSync(path.join(ROOT, 'node_modules'), path.join(workspace, 'node_modules'), 'dir');
  mkdirSync(path.join(workspace, 'scripts'));
  symlinkSync(path.join(ROOT, 'scripts/lint'), path.join(workspace, 'scripts/lint'), 'dir');
});

afterAll(() => {
  rmSync(workspace, { recursive: true, force: true });
});

describe('UI lint boundaries (node)', () => {
  it('rejects @tauri-apps imports outside src/ui/src/ipc', () => {
    const result = probe(
      'src/ui/src/windows/Probe/Probe.ts',
      "import { invoke } from '@tauri-apps/api/core';\n\nexport const probe = invoke;\n",
    );

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('no-restricted-imports');
  });

  it('rejects generated binding imports outside src/ui/src/ipc', () => {
    const result = probe(
      'src/ui/src/hooks/probe.ts',
      "import { commands } from '../ipc/bindings.gen';\n\nexport const probe = commands;\n",
    );

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('no-restricted-imports');
  });

  it.each(['src/ui/src/ipc/probe.ts', 'src/ui/tests/probe.ts', 'src/ui/e2e/probe.ts'])(
    'allows @tauri-apps imports in %s',
    (relative) => {
      const result = probe(
        relative,
        "import { invoke } from '@tauri-apps/api/core';\n\nexport const probe = invoke;\n",
      );

      expect(result.output).not.toContain('no-restricted-imports');
      expect(result.status).toBe(0);
    },
  );

  it('rejects an invalid StyleX property', () => {
    const result = probe(
      'src/ui/src/windows/Probe/Probe.style.ts',
      [
        "import * as stylex from '@stylexjs/stylex';",
        '',
        "export const styles = stylex.create({ root: { colr: 'red' } });",
        '',
      ].join('\n'),
    );

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('valid-styles');
  });

  it.each([
    [
      'src/ui/src/components/Probe/Probe.style.ts',
      "import { latte } from '@/theme/palette.stylex';",
    ],
    ['src/ui/src/windows/Probe/Probe.tsx', "import { latte } from '../../theme/palette.stylex';"],
  ])('rejects a palette import from %s', (relative, line) => {
    const result = probe(relative, `${line}\n\nexport const probe = latte;\n`);

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('no-restricted-imports');
  });

  it('allows a palette import inside src/ui/src/theme', () => {
    const result = probe(
      'src/ui/src/theme/probe.stylex.ts',
      [
        "import * as stylex from '@stylexjs/stylex';",
        '',
        "import { latte } from './palette.stylex';",
        '',
        'export const probe = stylex.defineVars({ surface: latte.base });',
        '',
      ].join('\n'),
    );

    expect(result.output).not.toContain('no-restricted-imports');
    expect(result.status).toBe(0);
  });

  it.each(['src/ui/src/components/Probe/Probe.tsx', 'src/ui/src/theme/probe.ts'])(
    'rejects the full motion component in %s',
    (relative) => {
      const result = probe(
        relative,
        "import { motion } from 'motion/react';\n\nexport const probe = motion;\n",
      );

      expect(result.status).not.toBe(0);
      expect(result.output).toContain('no-restricted-imports');
    },
  );

  it('allows motion/react helpers and the m components', () => {
    const result = probe(
      'src/ui/src/components/Probe/Probe.tsx',
      [
        "import { LazyMotion } from 'motion/react';",
        "import * as m from 'motion/react-m';",
        '',
        'export const probe = [LazyMotion, m];',
        '',
      ].join('\n'),
    );

    expect(result.output).not.toContain('no-restricted-imports');
    expect(result.status).toBe(0);
  });

  it.each(['src/ui/src/ipc/probe.ts', 'src/ui/tests/probe.ts', 'src/ui/e2e/probe.ts'])(
    'still rejects the full motion component, framer-motion and the palette in %s',
    (relative) => {
      for (const line of [
        "import { motion } from 'motion/react';",
        "import { motion } from 'framer-motion';",
        "import { latte as motion } from '@/theme/palette.stylex';",
      ]) {
        const result = probe(relative, `${line}\n\nexport const probe = motion;\n`);

        expect(result.status, line).not.toBe(0);
        expect(result.output, line).toContain('no-restricted-imports');
      }
    },
  );

  const style = (body: string) =>
    ["import * as stylex from '@stylexjs/stylex';", '', body, ''].join('\n');

  const styleProbe = (body: string) =>
    probe('src/ui/src/windows/Probe/Probe.style.ts', style(body));

  it('rejects StyleX keys that are not in the clean property order', () => {
    const result = styleProbe(
      "export const styles = stylex.create({ root: { color: 'red', display: 'grid' } });",
    );

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('sort-keys');
  });

  it('accepts StyleX keys in the clean property order', () => {
    const result = styleProbe(
      "export const styles = stylex.create({ root: { display: 'grid', color: 'red' } });",
    );

    expect(result.output).toBe('');
    expect(result.status).toBe(0);
  });

  it.each([
    [
      'a layout property in transitionProperty',
      "{ transitionProperty: 'opacity, width' }",
      'width',
    ],
    ['all in transitionProperty', "{ transitionProperty: 'all' }", 'all'],
    [
      'a layout property in a transition shorthand',
      "{ transition: 'height 200ms ease' }",
      'height',
    ],
    ['a transition shorthand without a property', "{ transition: '200ms ease-out' }", 'all'],
    ['a camelCase layout property', "{ transitionProperty: 'marginTop' }", 'marginTop'],
    [
      'one transition of several without a property',
      "{ transition: 'opacity 1s, 200ms ease' }",
      'all',
    ],
    [
      'a layout property in a conditional value',
      "{ transitionProperty: { default: 'opacity', ':hover': 'margin-top' } }",
      'margin-top',
    ],
  ])('rejects %s', (_name, declaration, property) => {
    const result = styleProbe(`export const styles = stylex.create({ root: ${declaration} });`);

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('no-low-performance-animation-properties');
    expect(result.output).toContain(`(${property})`);
  });

  it('rejects a layout property inside stylex.keyframes', () => {
    const result = styleProbe(
      [
        'const grow = stylex.keyframes({ from: { maxHeight: 0 }, to: { maxHeight: 100 } });',
        '',
        'export const styles = stylex.create({ root: { animationName: grow } });',
      ].join('\n'),
    );

    expect(result.status).not.toBe(0);
    expect(result.output).toContain('animation property (max-height)');
  });

  it('accepts compositor and paint properties in transitions and keyframes', () => {
    const result = styleProbe(
      [
        'const fade = stylex.keyframes({ from: { opacity: 0 }, to: { opacity: 1 } });',
        '',
        'export const styles = stylex.create({',
        '  root: {',
        "    transitionDuration: '200ms',",
        "    transitionProperty: 'opacity, transform, background-color',",
        '    animationName: fade,',
        '  },',
        "  explicit: { transition: 'all 1s', transitionProperty: 'opacity' },",
        '});',
      ].join('\n'),
    );

    expect(result.output).toBe('');
    expect(result.status).toBe(0);
  });
});
