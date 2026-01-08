import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Clock } from './Clock';

describe('Clock Component', () => {
  test('renders clock time', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['clock'], 'Thu Dec 18 14:30:45');

    const { getByText } = await render(<Clock />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Thu Dec 18 14:30:45')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders nothing when clock is not available', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['clock'], null);

    const { container } = await render(<Clock />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Should not render button when no clock data
      expect(container.querySelector('button')).toBeNull();
    });

    queryClient.clear();
  });

  test('renders formatted date and time', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['clock'], 'Mon Jan 01 00:00:00');

    const { getByText } = await render(<Clock />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Mon Jan 01 00:00:00')).toBeDefined();
    });

    queryClient.clear();
  });
});
