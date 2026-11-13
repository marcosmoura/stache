import { describe, expect, it } from 'vitest';

import { render } from '../../../../../tests/render';

import { Troubleshooting } from './Troubleshooting';

describe('Troubleshooting', () => {
  it('starts collapsed and toggles on its button', async () => {
    const screen = await render(<Troubleshooting bundled />);
    const toggle = screen.getByRole('button', { name: 'Troubleshooting' });

    await expect.element(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.container.textContent).not.toContain('older copy of the app');
    await toggle.click();
    await expect.element(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect
      .element(screen.getByText(/That entry is from an older copy of the app/))
      .toBeVisible();
    await expect
      .poll(() => getComputedStyle(toggle.element().querySelector('[data-chevron]')!).transform)
      .not.toBe('none');
    await toggle.click();
    expect(screen.container.textContent).not.toContain('older copy of the app');
  });

  it('explains an unbundled binary', async () => {
    const screen = await render(<Troubleshooting bundled={false} />);

    await screen.getByRole('button', { name: 'Troubleshooting' }).click();
    await expect.element(screen.getByText(/isn't installed as a regular app/)).toBeVisible();
  });
});
