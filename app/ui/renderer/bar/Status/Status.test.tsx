import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Status } from './Status';

describe('Status Component', () => {
  test('renders status container with child components', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Status />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('div')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders battery info', async () => {
    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Status />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('75% (Discharging)')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders cpu info', async () => {
    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Status />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('30%')).toBeDefined();
    });

    queryClient.clear();
  });
});
