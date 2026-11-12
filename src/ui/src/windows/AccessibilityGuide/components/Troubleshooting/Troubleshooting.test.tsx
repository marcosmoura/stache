import { describe, expect, it } from 'vitest';

import { render } from '../../../../../tests/render';

import { Troubleshooting } from './Troubleshooting';

describe('Troubleshooting', () => {
  it('starts collapsed and toggles on its button', async () => {
    const screen = await render(<Troubleshooting bundled />);
    const toggle = screen.getByRole('button', { name: 'Troubleshooting' });

    await expect.element(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.container.textContent).not.toContain('older build');
    await toggle.click();
    await expect.element(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect.element(screen.getByText(/The entry belongs to an older build/)).toBeVisible();
    await expect
      .poll(() => getComputedStyle(toggle.element().querySelector('[data-chevron]')!).transform)
      .not.toBe('none');
    await toggle.click();
    expect(screen.container.textContent).not.toContain('older build');
  });

  it('explains an unbundled binary', async () => {
    const screen = await render(<Troubleshooting bundled={false} />);

    await screen.getByRole('button', { name: 'Troubleshooting' }).click();
    await expect.element(screen.getByText(/This copy is not an app bundle\./)).toBeVisible();
  });
});
