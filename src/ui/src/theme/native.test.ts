import { describe, expect, it } from 'vitest';

/*
 * TP-05-01: the popover and the Accessibility guide have a native macOS look,
 * so neither their sources nor the `native`, `popover` and `guide` token groups
 * may use the Catppuccin palette, directly or through the palette-backed
 * `color` and `font` groups.
 */

const SOURCES = import.meta.glob<string>(
  ['../windows/Popover/**/*.{ts,tsx}', '../windows/AccessibilityGuide/**/*.{ts,tsx}'],
  { eager: true, import: 'default', query: '?raw' },
);

const TOKENS = Object.values(
  import.meta.glob<string>('./tokens.stylex.ts', {
    eager: true,
    import: 'default',
    query: '?raw',
  }),
)[0];

const sources = Object.entries(SOURCES).filter(([path]) => !/\.test\.tsx?$/.test(path));

/** The named imports of `@/theme/tokens.stylex` in one source. */
const tokenImports = (source: string) =>
  [...source.matchAll(/import\s*\{([^}]*)\}\s*from\s*'@\/theme\/tokens\.stylex'/g)].flatMap(
    (match) => match[1].split(',').map((name) => name.trim()),
  );

/** The source of one `export const <name> = stylex.defineVars({...})` group. */
function group(name: string) {
  const start = TOKENS.indexOf(`export const ${name} = `);
  const open = TOKENS.indexOf('{', start);
  let depth = 0;
  let end = open;

  for (; end < TOKENS.length; end += 1) {
    depth += TOKENS[end] === '{' ? 1 : TOKENS[end] === '}' ? -1 : 0;

    if (depth === 0) {
      break;
    }
  }

  expect(start).toBeGreaterThan(-1);
  expect(end).toBeLessThan(TOKENS.length);

  return TOKENS.slice(start, end + 1);
}

describe('native look', () => {
  it('scans every popover and guide source', () => {
    expect(sources.filter(([path]) => path.includes('/Popover/')).length).toBeGreaterThan(10);
    expect(
      sources.filter(([path]) => path.includes('/AccessibilityGuide/')).length,
    ).toBeGreaterThan(10);
  });

  it.each(sources)('%s uses no palette and no `color` token', (_path, source) => {
    expect(source).not.toMatch(/palette\.stylex/);
    expect(source).not.toMatch(/\b(latte|mocha)\b/);
    expect(tokenImports(source)).not.toContain('color');
    expect(tokenImports(source)).not.toContain('font');
    expect(source).not.toMatch(/\bcolor\.[a-zA-Z]/);
    expect(source).not.toMatch(/\bfont\.[a-zA-Z]/);
  });

  it.each(['native', 'popover', 'guide'])('the `%s` token group uses no palette', (name) => {
    const source = group(name);

    expect(source).not.toMatch(/\b(latte|mocha)\b/);
    expect(source).not.toMatch(/\b(color|font)\./);
  });

  it('the `native` token group uses system colours and the system font', () => {
    const source = group('native');

    expect(source).toContain('-apple-system-label');
    expect(source).toContain('-apple-system-control-accent');
    expect(source).toMatch(/family: '-apple-system/);
  });
});
