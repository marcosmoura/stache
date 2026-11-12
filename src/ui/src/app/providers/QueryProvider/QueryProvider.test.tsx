import { useQueryClient, type QueryClient } from '@tanstack/react-query';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { QueryProvider } from './QueryProvider';

describe('QueryProvider', () => {
  it('gives each mounted provider its own client and keeps it across renders', async () => {
    const seen: Record<string, QueryClient[]> = { first: [], second: [] };

    function Probe({ name }: { name: string }) {
      const client = useQueryClient();

      seen[name]?.push(client);

      return <span>{name}</span>;
    }

    const tree = (label: string) => (
      <>
        <QueryProvider>
          <Probe name="first" />
        </QueryProvider>
        <QueryProvider>
          <Probe name="second" />
        </QueryProvider>
        <i>{label}</i>
      </>
    );

    const screen = await render(tree('one'));

    await screen.rerender(tree('two'));
    await expect.element(screen.getByText('two')).toBeVisible();

    expect(new Set(seen.first).size).toBe(1);
    expect(new Set(seen.second).size).toBe(1);
    expect(seen.first?.[0]).not.toBe(seen.second?.[0]);
  });

  it('uses the local IPC defaults', async () => {
    let client: QueryClient | undefined;

    function Probe() {
      client = useQueryClient();

      return null;
    }

    await render(
      <QueryProvider>
        <Probe />
      </QueryProvider>,
    );

    const defaults = client?.getDefaultOptions();

    expect(defaults?.queries).toMatchObject({
      networkMode: 'always',
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
      retry: false,
      staleTime: Infinity,
    });
    expect(defaults?.mutations).toMatchObject({ networkMode: 'always', retry: false });
  });
});
