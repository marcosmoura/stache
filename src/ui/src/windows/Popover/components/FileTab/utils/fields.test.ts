import { describe, expect, it } from 'vitest';

import { node } from '../../../../../../tests/settingsForm';

import { numberError, ordered, variantOptions } from './fields';

describe('fields', () => {
  it('orders keys by the copy and drops constants', () => {
    const tree = node.object({ a: node.string(), tag: node.const('x'), b: node.boolean() });

    expect(ordered(tree.properties, ['b']).map((property) => property.key)).toEqual(['b', 'a']);
  });

  it('explains a number that cannot be saved', () => {
    const ratio = node.number(0, 100) as Parameters<typeof numberError>[0];

    expect(numberError(ratio, '')).toBe('Enter a number.');
    expect(numberError(ratio, '1.5')).toBe('Enter a whole number.');
    expect(numberError(ratio, '-1')).toBe('Must be at least 0.');
    expect(numberError(ratio, '101')).toBe('Must be at most 100.');
    expect(numberError(ratio, '60')).toBeNull();
  });

  it('names union forms from their copy, else humanised', () => {
    const union = node.union({ plain_text: node.string(), table: node.object({}) });

    expect(
      variantOptions(union.variants, { table: { label: 'Table', hint: 'Many keys' } }),
    ).toEqual([
      { value: '0', title: 'Plain text', hint: null },
      { value: '1', title: 'Table', hint: 'Many keys' },
    ]);
  });
});
