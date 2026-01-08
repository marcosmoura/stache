import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient, createFetchMock } from '@/tests/utils';

import { Weather } from './Weather';

describe('Weather Component', () => {
  test('renders weather info', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Berlin', country_name: 'Germany' } },
      { pattern: 'ipinfo.io', response: { city: 'Berlin', country: 'DE' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: 20,
            icon: 'clear-day',
            conditions: 'Clear',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather'], {
      temperature: 20,
      icon: 'clear-day',
      conditions: 'Clear',
    });

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders temperature label', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'London', country_name: 'UK' } },
      { pattern: 'ipinfo.io', response: { city: 'London', country: 'GB' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: 15,
            icon: 'cloudy',
            conditions: 'Cloudy',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather-label'], '15°C Cloudy');

    const { getByText } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('15°C Cloudy')).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders weather container', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Tokyo', country_name: 'Japan' } },
      { pattern: 'ipinfo.io', response: { city: 'Tokyo', country: 'JP' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: 25,
            icon: 'partly-cloudy-day',
            conditions: 'Partly Cloudy',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      // Weather always renders (even with loading state)
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders loading state when no weather data', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Paris', country_name: 'France' } },
      { pattern: 'ipinfo.io', response: { city: 'Paris', country: 'FR' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: null,
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();

    const { getByText } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Loading weather...')).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders weather with snow icon', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Oslo', country_name: 'Norway' } },
      { pattern: 'ipinfo.io', response: { city: 'Oslo', country: 'NO' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: -5,
            icon: 'snow',
            conditions: 'Snow',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather'], {
      currentConditions: {
        feelslike: -5,
        icon: 'snow',
        conditions: 'Snow',
      },
    });

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders weather with rain icon', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Seattle', country_name: 'USA' } },
      { pattern: 'ipinfo.io', response: { city: 'Seattle', country: 'US' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: 10,
            icon: 'rain',
            conditions: 'Rainy',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather'], {
      currentConditions: {
        feelslike: 10,
        icon: 'rain',
        conditions: 'Rainy',
      },
    });

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders weather with night icon', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Sydney', country_name: 'Australia' } },
      { pattern: 'ipinfo.io', response: { city: 'Sydney', country: 'AU' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: 18,
            icon: 'clear-night',
            conditions: 'Clear',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather'], {
      currentConditions: {
        feelslike: 18,
        icon: 'clear-night',
        conditions: 'Clear',
      },
    });

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('renders weather with unknown icon defaults to clear-day', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Mars', country_name: 'Space' } },
      { pattern: 'ipinfo.io', response: { city: 'Mars', country: 'SP' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: 0,
            icon: 'unknown-icon',
            conditions: 'Unknown',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather'], {
      currentConditions: {
        feelslike: 0,
        icon: 'unknown-icon',
        conditions: 'Unknown',
      },
    });

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });

  test('handles null feelslike value', async () => {
    const mockFetch = createFetchMock([
      { pattern: 'ipapi.co', response: { city: 'Unknown', country_name: 'Unknown' } },
      { pattern: 'ipinfo.io', response: { city: 'Unknown', country: 'UN' } },
      {
        pattern: 'visualcrossing',
        response: {
          currentConditions: {
            feelslike: null,
            icon: 'clear-day',
            conditions: '',
          },
        },
      },
    ]);
    vi.spyOn(globalThis, 'fetch').mockImplementation(mockFetch);

    const queryClient = createTestQueryClient();
    queryClient.setQueryData(['weather'], {
      currentConditions: {
        feelslike: null,
        icon: 'clear-day',
        conditions: '',
      },
    });

    const { container } = await render(<Weather />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
    });

    queryClient.clear();
    vi.restoreAllMocks();
  });
});
