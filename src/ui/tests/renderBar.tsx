import type { ReactNode } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render } from 'vitest-browser-react';

import type { BarData } from '../src/shell/windows/Bar/Bar.data';
import { BarDataProvider } from '../src/shell/windows/Bar/BarDataProvider';

import { absentBarData } from './barFixtures';

/** Renders Bar UI against a fixed data set (the static fixture seam). */
export async function renderWithBarData(ui: ReactNode, data: BarData = absentBarData) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });

  const screen = await render(
    <QueryClientProvider client={client}>
      <BarDataProvider data={data}>{ui}</BarDataProvider>
    </QueryClientProvider>,
  );

  return { client, screen };
}
