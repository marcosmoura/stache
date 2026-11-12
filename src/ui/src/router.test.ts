import { describe, expect, it } from 'vitest';

import { DEFAULT_SURFACE, resolveRoute, surfaces, type Surface } from './router';

const known: Surface[] = ['shell', 'test', 'access', 'bar'];

describe('resolveRoute', () => {
  it.each(known)('resolves ?surface=%s to its window', (surface) => {
    const route = resolveRoute(`?surface=${surface}`);

    expect(route).toEqual({ kind: 'window', surface, Window: surfaces[surface] });
  });

  it('lists exactly the four surfaces', () => {
    expect(Object.keys(surfaces).sort()).toEqual([...known].sort());
  });

  it('defaults to shell when the parameter is absent', () => {
    expect(DEFAULT_SURFACE).toBe('shell');
    expect(resolveRoute('')).toMatchObject({ kind: 'window', surface: 'shell' });
    expect(resolveRoute('?other=bar')).toMatchObject({ kind: 'window', surface: 'shell' });
  });

  it('falls back on an empty value', () => {
    expect(resolveRoute('?surface=')).toMatchObject({ kind: 'fallback', requested: '' });
    expect(resolveRoute('?surface')).toMatchObject({ kind: 'fallback', requested: '' });
  });

  it('falls back on an unknown value', () => {
    expect(resolveRoute('?surface=settings')).toMatchObject({
      kind: 'fallback',
      requested: 'settings',
    });
    expect(resolveRoute('?surface=toString')).toMatchObject({ kind: 'fallback' });
    expect(resolveRoute('?surface=__proto__')).toMatchObject({ kind: 'fallback' });
  });

  it('uses the first value when the parameter repeats', () => {
    expect(resolveRoute('?surface=bar&surface=test')).toMatchObject({ surface: 'bar' });
    expect(resolveRoute('?surface=nope&surface=bar')).toMatchObject({
      kind: 'fallback',
      requested: 'nope',
    });
  });

  it('matches case-sensitively', () => {
    expect(resolveRoute('?surface=Bar')).toMatchObject({ kind: 'fallback', requested: 'Bar' });
    expect(resolveRoute('?surface=SHELL')).toMatchObject({ kind: 'fallback' });
  });

  it('maps bar, test and access to their own windows and shell to the Placeholder', () => {
    expect(new Set(known.map((surface) => surfaces[surface])).size).toBe(known.length);
  });
});
