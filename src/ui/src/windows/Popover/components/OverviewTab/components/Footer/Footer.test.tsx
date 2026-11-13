import { describe, expect, it, vi } from 'vitest';

import { PRODUCT_NAME } from '@/identity';

import { mockHost } from '../../../../../../../tests/ipc';
import { render } from '../../../../../../../tests/render';

import { Footer } from './Footer';

const APP = { name: 'Stache Next', version: '0.0.0', identifier: 'com.example.stache' };

describe('Footer', () => {
  it('quits from its action', async () => {
    const host = mockHost().answer('app_info', APP).answer('quit_app', null);
    const screen = await render(<Footer />);

    await expect.element(screen.getByRole('navigation', { name: 'Actions' })).toBeVisible();
    await screen.getByRole('button', { name: `Quit ${PRODUCT_NAME}` }).click();
    await vi.waitFor(() => expect(host.callsOf('quit_app')).toHaveLength(1));
  });

  it('names the app with the name the host reports', async () => {
    mockHost().answer('app_info', { ...APP, name: 'Injected Name' });
    const screen = await render(<Footer />);

    await expect.element(screen.getByRole('button', { name: 'Quit Injected Name' })).toBeVisible();
  });

  it('falls back to the product name before or without the host answer', async () => {
    const host = mockHost();
    const screen = await render(<Footer />);

    await vi.waitFor(() => expect(host.callsOf('app_info')).toHaveLength(1));
    await expect.element(screen.getByRole('button', { name: 'Quit Stache Next' })).toBeVisible();
    expect(PRODUCT_NAME).toBe('Stache Next');
  });
});
