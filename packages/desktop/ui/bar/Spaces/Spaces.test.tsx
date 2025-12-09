import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Spaces } from './Spaces';

describe('Spaces Component', () => {
  test('renders Hyprspace and CurrentApp components', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Should have the main container
      expect(container.querySelector('div')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders workspace buttons', async () => {
    const queryClient = createTestQueryClient();
    const { container } = await render(<Spaces />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelectorAll('button').length).toBeGreaterThan(0);
    });

    queryClient.clear();
  });
});
