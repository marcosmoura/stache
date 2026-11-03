import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Cpu } from './Cpu';

describe('Cpu Component', () => {
  test('renders cpu usage', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['cpu'], {
      usage: 30,
      temperature: null,
    });

    const { getByText } = await render(<Cpu />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('30%')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders cpu usage with temperature', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['cpu'], {
      usage: 45,
      temperature: 65,
    });

    const { getByText } = await render(<Cpu />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('45%')).toBeDefined();
      expect(getByText('65°C')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders zero usage when no data', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['cpu'], null);

    const { getByText } = await render(<Cpu />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('0%')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders high temperature cpu info', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['cpu'], {
      usage: 95,
      temperature: 90,
    });

    const { getByText } = await render(<Cpu />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('95%')).toBeDefined();
      expect(getByText('90°C')).toBeDefined();
    });

    queryClient.clear();
  });
});
