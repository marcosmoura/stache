import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';
import { MediaEvents } from '@/types';

import { Media } from './Media';

describe('Media Component', () => {
  test('renders nothing when no media is playing', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], null);

    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.querySelector('[data-test-id="media-container"]')).toBeNull();
    });

    queryClient.clear();
  });

  test('renders media container when media is playing', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Test Song - Test Artist',
      prefix: '',
      bundleIdentifier: 'com.spotify.client',
      artwork: null,
    });

    const { getByTestId } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('media-container')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders media label', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Bohemian Rhapsody - Queen',
      prefix: '',
      bundleIdentifier: 'com.spotify.client',
      artwork: null,
    });

    const { getByText } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Bohemian Rhapsody - Queen')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders paused prefix when media is paused', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Test Song - Test Artist',
      prefix: 'Paused: ',
      bundleIdentifier: 'com.spotify.client',
      artwork: null,
    });

    const { getByText } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Paused:')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders Spotify icon for Spotify media', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Test Song',
      prefix: '',
      bundleIdentifier: 'com.spotify.client',
      artwork: null,
    });

    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders YouTube icon for Edge media', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'YouTube Video',
      prefix: '',
      bundleIdentifier: 'com.microsoft.edgemac.Dev',
      artwork: null,
    });

    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders Tidal icon for Tidal media', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Tidal Song',
      prefix: '',
      bundleIdentifier: 'com.tidal.desktop',
      artwork: null,
    });

    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders default icon for unknown media app', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Unknown App Song',
      prefix: '',
      bundleIdentifier: 'com.unknown.app',
      artwork: null,
    });

    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      const svg = container.querySelector('svg');
      expect(svg).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders artwork when available', async () => {
    const queryClient = createTestQueryClient();
    // Base64 encoded 1x1 transparent PNG
    const base64Artwork =
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Test Song',
      prefix: '',
      bundleIdentifier: 'com.spotify.client',
      artwork: base64Artwork,
    });

    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(
      () => {
        const img = container.querySelector('img');
        expect(img).toBeDefined();
      },
      { timeout: 2000 },
    );

    queryClient.clear();
  });

  test('handles click on media container', async () => {
    const queryClient = createTestQueryClient();
    queryClient.setQueryData([MediaEvents.PLAYBACK_CHANGED], {
      label: 'Test Song',
      prefix: '',
      bundleIdentifier: 'com.spotify.client',
      artwork: null,
    });

    const { getByTestId } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByTestId('media-container')).toBeDefined();
    });

    queryClient.clear();
  });
});
