import { describe, expect, it, vi } from 'vitest';

import type { HostStatus } from '@/ipc';

import { hostStatusFixture } from '../../../../../../../tests/fixtures';
import { mockHost } from '../../../../../../../tests/ipc';
import { render } from '../../../../../../../tests/render';

import { AccessBanner } from './AccessBanner';

const blocked = (revoked: boolean) =>
  hostStatusFixture({ access: { state: 'blocked', revoked, appPath: null } }) as HostStatus;

describe('AccessBanner', () => {
  it('shows the title and text while blocked, and its first action opens the guide', async () => {
    const host = mockHost().answer('open_accessibility_guide', null);
    const screen = await render(<AccessBanner status={blocked(false)} />);

    await expect
      .element(screen.getByRole('region', { name: 'Accessibility required' }))
      .toBeVisible();
    await expect.element(screen.getByText(/Stache needs Accessibility/)).toBeVisible();
    await screen.getByRole('button', { name: 'Show Accessibility Help…' }).click();
    await vi.waitFor(() => expect(host.callsOf('open_accessibility_guide')).toHaveLength(1));
  });

  it('names a revoked permission', async () => {
    mockHost();
    const screen = await render(<AccessBanner status={blocked(true)} />);

    await expect
      .element(screen.getByRole('region', { name: 'Accessibility was turned off' }))
      .toBeVisible();
  });

  it('renders nothing while the gate is open', async () => {
    mockHost();
    const screen = await render(<AccessBanner status={hostStatusFixture() as HostStatus} />);

    expect(screen.container.textContent).toBe('');
  });
});
