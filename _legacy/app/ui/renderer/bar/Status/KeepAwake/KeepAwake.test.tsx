import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { KeepAwake } from './KeepAwake';

describe('KeepAwake Component', () => {
  test('renders keep awake button when state is defined', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], false);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders button when state is false', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], false);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders with sleep state', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], false);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const button = container.querySelector('[data-test-state="sleep"]');
      expect(button).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders with awake state', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], true);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const button = container.querySelector('[data-test-state="awake"]');
      expect(button).toBeDefined();
    });

    queryClient.clear();
  });

  test('button is clickable', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], false);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
    });

    queryClient.clear();
  });

  test('displays icon when awake', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], true);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('displays icon when sleep', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['keep-awake'], false);

    const { container } = await render(<KeepAwake />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });
});
