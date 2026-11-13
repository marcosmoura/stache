import { describe, expect, it } from 'vitest';

import { resolveRoute, surfaces, type Surface } from './router';

const known: Surface[] = ['access', 'bar', 'popover'];

describe('resolveRoute', () => {
  it.each(known)('resolves ?surface=%s to its window', (surface) => {
    const route = resolveRoute(`?surface=${surface}`);

    expect(route).toEqual({ kind: 'window', surface, Window: surfaces[surface] });
  });

  it('lists exactly the three surfaces', () => {
    expect(Object.keys(surfaces).sort()).toEqual([...known].sort());
  });

  it('falls back when the parameter is absent, since there is no default window', () => {
    expect(resolveRoute('')).toMatchObject({ kind: 'fallback', requested: '' });
    expect(resolveRoute('?other=bar')).toMatchObject({ kind: 'fallback', requested: '' });
    expect(resolveRoute('?surface=shell')).toMatchObject({ kind: 'fallback', requested: 'shell' });
    expect(resolveRoute('?surface=test')).toMatchObject({ kind: 'fallback', requested: 'test' });
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
    expect(resolveRoute('?surface=ACCESS')).toMatchObject({ kind: 'fallback' });
  });

  it('maps bar, access and popover to their own windows', () => {
    expect(new Set(known.map((surface) => surfaces[surface])).size).toBe(known.length);
  });
});
