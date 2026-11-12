import { afterEach, describe, expect, it, vi } from 'vitest';

import { renderWithSnapshot } from '../../../../../../tests/barSnapshot';
import { barAvailable, tilingStrip } from '../../../../../../tests/fixtures';

import { App } from './App';

afterEach(() => {
  vi.restoreAllMocks();
});

const apps = [
  { key: '1', appName: 'Ghostty', title: 'zsh' },
  { key: '2', appName: 'Code', title: 'a.ts' },
];
const outcome = barAvailable('1', { tiling: tilingStrip({ apps, focusedApp: apps[0] }) });

describe('App', () => {
  it('shows the label only while focused, and names an icon-only item by its label', async () => {
    const { screen } = await renderWithSnapshot(
      <>
        <App windowKey="1" compact={false} onFocus={vi.fn()} />
        <App windowKey="2" compact={false} onFocus={vi.fn()} />
      </>,
      outcome,
    );

    await expect
      .element(screen.getByRole('button', { name: 'Ghostty' }))
      .toHaveTextContent('Ghostty');
    await expect.element(screen.getByRole('button', { name: 'Code' })).toHaveTextContent('');

    const label = document.querySelector<HTMLElement>('[data-label]')!;

    expect(getComputedStyle(label).whiteSpace).toBe('nowrap');
    expect(getComputedStyle(label).overflow).toBe('hidden');
    expect(screen.container.querySelectorAll('svg')).toHaveLength(2);
  });

  it('sends its window key on click', async () => {
    const onFocus = vi.fn();
    const { screen } = await renderWithSnapshot(
      <App windowKey="2" compact={false} onFocus={onFocus} />,
      outcome,
    );

    await screen.getByRole('button', { name: 'Code' }).click();
    expect(onFocus).toHaveBeenCalledWith('2');
  });

  it('renders nothing for a key the strip does not list', async () => {
    const { screen } = await renderWithSnapshot(
      <App windowKey="9" compact={false} onFocus={vi.fn()} />,
      outcome,
    );

    await new Promise((done) => setTimeout(done, 20));
    expect(screen.container.childElementCount).toBe(0);
  });
});
