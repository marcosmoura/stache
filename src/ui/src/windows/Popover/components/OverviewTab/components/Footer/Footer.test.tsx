import { describe, expect, it, vi } from 'vitest';

import { mockHost } from '../../../../../../../tests/ipc';
import { render } from '../../../../../../../tests/render';

import { Footer } from './Footer';

describe('Footer', () => {
  it('quits from its action', async () => {
    const host = mockHost().answer('quit_app', null);
    const screen = await render(<Footer />);

    await expect.element(screen.getByRole('navigation', { name: 'Actions' })).toBeVisible();
    await screen.getByRole('button', { name: 'Quit Stache Next' }).click();
    await vi.waitFor(() => expect(host.callsOf('quit_app')).toHaveLength(1));
  });
});
