import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { Warnings } from './Warnings';

describe('Warnings', () => {
  it('lists each warning', async () => {
    const screen = await render(<Warnings warnings={['one', 'two']} />);

    await expect.element(screen.getByRole('region', { name: 'Warnings' })).toBeVisible();
    expect(screen.getByRole('listitem').elements()).toHaveLength(2);
  });

  it('renders nothing without warnings', async () => {
    const screen = await render(<Warnings warnings={[]} />);

    expect(screen.container.textContent).toBe('');
  });
});
