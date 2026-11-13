import { describe, expect, it, vi } from 'vitest';

import { PRODUCT_NAME } from '@/identity';

import { mockHost } from '../../../../../../../tests/ipc';
import { render } from '../../../../../../../tests/render';

import { Footer } from './Footer';

describe('Footer', () => {
  it('quits from its action', async () => {
    const host = mockHost().answer('quit_app', null);
    const screen = await render(<Footer />);

    await expect.element(screen.getByRole('navigation', { name: 'Actions' })).toBeVisible();
    await screen.getByRole('button', { name: `Quit ${PRODUCT_NAME}` }).click();
    await vi.waitFor(() => expect(host.callsOf('quit_app')).toHaveLength(1));
  });

  it('names the app with the product name fallback', () => {
    expect(PRODUCT_NAME).toBe('Stache Next');
  });
});
