import { DashboardCircleIcon } from '@hugeicons/core-free-icons';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Icon } from '@/components/Icon';

import { renderWithSnapshot } from '../../../../../../tests/barSnapshot';
import { barAvailable, tilingStrip } from '../../../../../../tests/fixtures';
import { render } from '../../../../../../tests/render';

import { Workspace } from './Workspace';

/* Catppuccin surface1 (Mocha, Latte). */
const SURFACE_ACTIVE = /rgb\(69, 71, 90\)|rgb\(188, 192, 204\)/;

afterEach(() => {
  vi.restoreAllMocks();
});

const outcome = barAvailable('1', { tiling: tilingStrip({ focusedWorkspace: 'terminal' }) });

describe('Workspace', () => {
  it('is an icon-only button named by the workspace, active while focused', async () => {
    const { screen } = await renderWithSnapshot(
      <>
        <Workspace name="terminal" highlightId="h" onFocus={vi.fn()} />
        <Workspace name="coding" highlightId="h" onFocus={vi.fn()} />
      </>,
      outcome,
    );
    const terminal = screen.getByRole('button', { name: 'terminal' });

    await expect.element(terminal).toHaveAttribute('aria-pressed', 'true');
    await expect.element(terminal).toHaveAttribute('data-active', 'true');
    await expect
      .element(screen.getByRole('button', { name: 'coding' }))
      .toHaveAttribute('aria-pressed', 'false');
    expect(terminal.element().textContent).toBe('');

    const indicator = terminal.element().querySelector<HTMLElement>('[data-highlight]')!;
    const style = getComputedStyle(indicator);

    expect(style.position).toBe('absolute');
    expect(style.borderTopLeftRadius).toBe('12px');
    expect(style.backgroundColor).toMatch(SURFACE_ACTIVE);
    expect(getComputedStyle(terminal.element()).paddingLeft).toBe('10px');
    expect(terminal.element().querySelector('svg')!.getBoundingClientRect().width).toBe(18);
  });

  it('sends its name on click', async () => {
    const onFocus = vi.fn();
    const { screen } = await renderWithSnapshot(
      <Workspace name="coding" highlightId="h" onFocus={onFocus} />,
      outcome,
    );

    await screen.getByRole('button', { name: 'coding' }).click();
    expect(onFocus).toHaveBeenCalledWith('coding');
  });

  it('draws the default icon for an unknown name', async () => {
    const { screen } = await renderWithSnapshot(
      <Workspace name="workspace-1" highlightId="h" onFocus={vi.fn()} />,
      outcome,
    );
    const expected = await render(<Icon icon={DashboardCircleIcon} />);
    const button = screen.getByRole('button', { name: 'workspace-1' }).element();

    expect(button.querySelector('svg path')?.getAttribute('d')).toBe(
      expected.container.querySelector('svg path')?.getAttribute('d'),
    );
  });
});
