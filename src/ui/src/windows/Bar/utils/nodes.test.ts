import { describe, expect, it } from 'vitest';

import { createWidgetNodes } from './nodes';

describe('createWidgetNodes', () => {
  it('creates one node per id on first acquire and returns it after', () => {
    const nodes = createWidgetNodes();
    const node = nodes.acquire('clock');

    expect(node.dataset.widgetNode).toBe('clock');
    expect(nodes.acquire('clock')).toBe(node);
    expect(nodes.acquire('cpu')).not.toBe(node);
  });

  it('adopts a node as the node of an id', () => {
    const nodes = createWidgetNodes();
    const node = document.createElement('div');

    nodes.acquire('clock');
    nodes.adopt('clock', node);
    expect(nodes.acquire('clock')).toBe(node);
  });

  it('releases only the current node of an id', () => {
    const nodes = createWidgetNodes();
    const first = nodes.acquire('clock');

    nodes.release('clock', document.createElement('div'));
    expect(nodes.acquire('clock')).toBe(first);

    nodes.release('clock', first);
    const second = nodes.acquire('clock');

    expect(second).not.toBe(first);
    expect(second.dataset.widgetNode).toBe('clock');
  });
});
