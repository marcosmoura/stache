import { describe, expect, test } from 'vitest';

import { barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { KeepAwake } from './KeepAwake';

describe('KeepAwake', () => {
  test('renders nothing while the state is unresolved (legacy null branch)', async () => {
    const { screen } = await renderWithBarData(<KeepAwake />, barData({ keepAwake: undefined }));

    expect(screen.container.querySelector('button')).toBeNull();
  });

  test('renders the active state with a green filled coffee icon', async () => {
    const { screen } = await renderWithBarData(<KeepAwake />, barData({ keepAwake: true }));
    const button = screen.container.querySelector('button');
    const svg = screen.container.querySelector('svg');

    expect(button?.dataset.testState).toBe('awake');
    expect(svg?.getAttribute('color')).toBe('#a6e3a1');
    expect(svg?.getAttribute('fill')).toBe('#a6e3a1');
  });

  test('renders the inactive state with a transparent fill and the inherited color', async () => {
    const { screen } = await renderWithBarData(<KeepAwake />, barData({ keepAwake: false }));
    const button = screen.container.querySelector('button');
    const svg = screen.container.querySelector('svg');

    expect(button?.dataset.testState).toBe('sleep');
    expect(svg?.getAttribute('color')).toBe('currentColor');
    expect(svg?.getAttribute('fill')).toBe('transparent');
  });

  test('transitions stroke and fill over the legacy 150ms', async () => {
    const { screen } = await renderWithBarData(<KeepAwake />, barData({ keepAwake: false }));
    const svg = screen.container.querySelector('svg');

    if (!svg) {
      throw new Error('icon missing');
    }

    const style = getComputedStyle(svg);

    expect(style.transitionProperty).toBe('stroke, fill');
    expect(style.transitionDuration).toBe('0.15s');
  });
});
