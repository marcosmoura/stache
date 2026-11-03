import { expect, test } from 'vitest';
import { render } from 'vitest-browser-react';

import { Shell } from './Shell';

test('renders the development shell without native services', async () => {
  expect(document.body.dataset.testSetup).toBe('ready');
  const screen = await render(<Shell />);
  const main = document.querySelector('main');
  if (!main) throw new Error('Shell did not mount');
  expect(getComputedStyle(main).backgroundColor).toBe('rgb(30, 30, 46)');
  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
  await expect
    .element(screen.getByText('Tray-only foundation. No tools are enabled.'))
    .toBeVisible();
  await screen.rerender(<Shell />);
  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
});
