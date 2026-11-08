import { expect, test } from 'vitest';
import { render } from 'vitest-browser-react';

import { Placeholder } from './Placeholder';

test('renders the development shell without native services', async () => {
  expect(document.body.dataset.testSetup).toBe('ready');
  const screen = await render(<Placeholder />);
  const main = document.querySelector('main');

  if (!main) {
    throw new Error('Placeholder did not mount');
  }

  expect(getComputedStyle(main).backgroundColor).toBe('rgb(30, 30, 46)');
  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
  await expect
    .element(screen.getByText('Tray-only foundation. No tools are enabled.'))
    .toBeVisible();
  await screen.rerender(<Placeholder />);
  await expect.element(screen.getByRole('heading', { name: 'Stache Next' })).toBeVisible();
});
