import { describe, expect, it } from 'vitest';

/*
 * TP-05-01: the popover has a native macOS look, so neither its sources nor
 * the `popover` token group may use the Catppuccin palette, directly or
 * through the palette-backed `color` group.
 */

const SOURCES = import.meta.glob<string>('./**/*.{ts,tsx}', {
  eager: true,
  import: 'default',
  query: '?raw',
});

const TOKENS = Object.values(
  import.meta.glob<string>('../../theme/tokens.stylex.ts', {
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

describe('popover native look', () => {
  it('scans every popover source', () => {
    expect(sources.length).toBeGreaterThan(10);
  });

  it.each(sources)('%s uses no palette and no `color` token', (_path, source) => {
    expect(source).not.toMatch(/palette\.stylex/);
    expect(source).not.toMatch(/\b(latte|mocha)\b/);
    expect(tokenImports(source)).not.toContain('color');
    expect(source).not.toMatch(/\bcolor\.[a-zA-Z]/);
  });

  it('the `popover` token group uses system colours, not the palette', () => {
    const start = TOKENS.indexOf('export const popover = ');
    const open = TOKENS.indexOf('{', start);
    let depth = 0;
    let end = open;

    for (; end < TOKENS.length; end += 1) {
      depth += TOKENS[end] === '{' ? 1 : TOKENS[end] === '}' ? -1 : 0;

      if (depth === 0) {
        break;
      }
    }

    const group = TOKENS.slice(start, end + 1);

    expect(start).toBeGreaterThan(-1);
    expect(end).toBeLessThan(TOKENS.length);
    expect(group).not.toMatch(/\b(latte|mocha)\b/);
    expect(group).not.toMatch(/\bcolor\./);
    expect(group).toContain('-apple-system-label');
    expect(group).toContain('-apple-system-control-accent');
    expect(group).toMatch(/family: '-apple-system/);
  });
});
