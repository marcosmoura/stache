import { QueryClientProvider, useQuery } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import { render as renderInBrowser } from 'vitest-browser-react';

import { hostStatusFixture } from '../../tests/fixtures';
import { mockHost } from '../../tests/ipc';
import { createQueryClient } from '../../tests/queryClient';
import { render } from '../../tests/render';

import { appInfoOptions, hostStatusFeed, hostStatusOptions } from './host';

const appInfo = { name: 'Stache', version: '0.0.0', identifier: 'com.example.stache' };

function AppName() {
  const { data } = useQuery({ ...appInfoOptions(), select: (info) => info.name });

  return <span>{data ?? 'loading'}</span>;
}

describe('host queries', () => {
  it('builds stable option factories', () => {
    expect(appInfoOptions().queryKey).toEqual(['host', 'appInfo']);
    expect(appInfoOptions().staleTime).toBe('static');
    expect(hostStatusOptions().queryKey).toEqual(['host', 'status']);
    expect(hostStatusFeed.queryKey).toEqual(hostStatusOptions().queryKey);
  });

  it('fetches appInfo once and never refetches it', async () => {
    const host = mockHost();

    host.answer('app_info', appInfo);
    const screen = await render(
      <>
        <AppName />
        <AppName />
      </>,
    );

    await expect.element(screen.getByText('Stache').first()).toBeVisible();
    await screen.rerender(<AppName />);
    expect(host.callsOf('app_info')).toHaveLength(1);
  });

  it('does not refetch appInfo on invalidation while it is observed', async () => {
    const host = mockHost();

    host.answer('app_info', appInfo);
    const client = createQueryClient();
    const screen = await renderInBrowser(
      <QueryClientProvider client={client}>
        <AppName />
      </QueryClientProvider>,
    );

    await expect.element(screen.getByText('Stache')).toBeVisible();
    await client.invalidateQueries();
    await client.refetchQueries();
    expect(host.callsOf('app_info')).toHaveLength(1);
  });

  it('keeps the host status current from the feed', async () => {
    const host = mockHost();

    host.answer('host_status', hostStatusFixture());
    const client = createQueryClient();
    const stop = hostStatusFeed.start(client);

    await vi.waitFor(() =>
      expect(client.getQueryData(hostStatusOptions().queryKey)?.ready).toBe(true),
    );
    stop();
  });
});
