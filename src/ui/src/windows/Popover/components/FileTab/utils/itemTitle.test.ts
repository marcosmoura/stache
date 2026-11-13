import { describe, expect, it } from 'vitest';

import { itemTitle } from './itemTitle';

describe('itemTitle', () => {
  it('titles a card from a name, a widget, an id or the noun', () => {
    const widgets = { options: { clock: { label: 'Clock' } }, variants: { cpu: { label: 'CPU' } } };

    expect(itemTitle({ name: 'coding' }, 0, {}, 'Workspace')).toBe('coding');
    expect(itemTitle({ widget: 'cpu', id: 'c1' }, 0, widgets, 'Widget')).toBe('CPU');
    expect(itemTitle({ widget: 'x', id: 'c1' }, 0, widgets, 'Widget')).toBe('c1');
    expect(itemTitle('clock', 0, widgets, 'Widget')).toBe('Clock');
    expect(itemTitle({}, 2, {}, 'Rule')).toBe('Rule 3');
  });
});
