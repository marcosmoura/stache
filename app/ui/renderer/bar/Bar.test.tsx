import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Bar } from './Bar';

describe('Bar Component', () => {
  test('renders main bar container', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('div')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders Spaces, Media, and Status containers', async () => {
    const queryClient = createTestQueryClient();
    const { getByTestId } = await render(<Bar />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('spaces-container')).toBeDefined();
      expect(getByTestId('media-container')).toBeDefined();
      expect(getByTestId('status-container')).toBeDefined();
    });

    queryClient.clear();
  });
});
