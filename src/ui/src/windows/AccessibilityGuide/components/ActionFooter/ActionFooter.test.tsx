import { describe, expect, it, vi } from 'vitest';

import { render } from '../../../../../tests/render';

import { ActionFooter } from './ActionFooter';

describe('ActionFooter', () => {
  it('runs an action on click and disables a pending one with a spinner', async () => {
    const request = vi.fn();
    const quit = vi.fn();
    const screen = await render(
      <ActionFooter
        actions={[
          { id: 'request', label: 'Request Access', pending: false, onRun: request },
          { id: 'quit', label: 'Quit Stache', pending: true, onRun: quit },
        ]}
      />,
    );
    const primary = screen.getByRole('button', { name: 'Request Access' });
    const pending = screen.getByRole('button', { name: 'Quit Stache' });

    await primary.click();
    expect(request).toHaveBeenCalledOnce();
    await expect.element(pending).toBeDisabled();
    await expect.element(pending).toHaveAttribute('aria-busy', 'true');
    expect(pending.element().querySelector('[data-spinner]')).not.toBeNull();
    expect(primary.element().querySelector('[data-spinner]')).toBeNull();
    expect(quit).not.toHaveBeenCalled();
  });

  it('spans the footer with the primary action', async () => {
    const screen = await render(
      <ActionFooter
        actions={[
          { id: 'a', label: 'A', pending: false, onRun: () => {} },
          { id: 'b', label: 'B', pending: false, onRun: () => {} },
        ]}
      />,
    );
    const footer = screen.container.querySelector('footer')!;

    await expect.poll(() => getComputedStyle(footer).display).toBe('grid');
    expect(screen.getByRole('button', { name: 'A' }).element().getBoundingClientRect().width).toBe(
      footer.getBoundingClientRect().width -
        Number.parseFloat(getComputedStyle(footer).paddingLeft) * 2,
    );
  });
});
