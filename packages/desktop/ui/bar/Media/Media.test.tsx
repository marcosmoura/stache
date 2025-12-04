import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { colors } from '@/design-system';
import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { Media } from './Media';

import { fetchCurrentMedia, getPlayerIcon, openMediaApp, parseMediaPayload } from './Media.service';
import type { MediaPayload, TransformedMediaPayload } from './Media.types';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const invokeMock = vi.mocked(invoke);

describe('Media Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  describe('fetchCurrentMedia', () => {
    test('returns media info when invoke succeeds', async () => {
      const mockPayload: MediaPayload = {
        artist: 'Test Artist',
        title: 'Test Song',
        artwork: null,
        bundleIdentifier: 'com.test.app',
        playing: true,
      };
      invokeMock.mockResolvedValue(mockPayload);

      const result = await fetchCurrentMedia();

      expect(invokeMock).toHaveBeenCalledWith('get_current_media_info');
      expect(result).toEqual(mockPayload);
    });

    test('returns null when no media is playing', async () => {
      invokeMock.mockResolvedValue(undefined);

      const result = await fetchCurrentMedia();

      expect(result).toBeNull();
    });
  });

  describe('openMediaApp', () => {
    test('invokes open_app for spotify', async () => {
      invokeMock.mockResolvedValue(undefined);
      const media: TransformedMediaPayload = {
        label: 'Test Song',
        prefix: '',
        bundleIdentifier: 'com.spotify.client',
      };

      await openMediaApp(media);

      expect(invokeMock).toHaveBeenCalledWith('open_app', { name: 'Spotify' });
    });

    test('does nothing for unknown bundle', async () => {
      const media: TransformedMediaPayload = {
        label: 'Test Song',
        prefix: '',
        bundleIdentifier: 'com.unknown.app',
      };

      await openMediaApp(media);

      expect(invokeMock).not.toHaveBeenCalled();
    });

    test('does nothing when media is undefined', async () => {
      await openMediaApp(undefined);

      expect(invokeMock).not.toHaveBeenCalled();
    });
  });

  describe('getPlayerIcon', () => {
    test('returns spotify icon for spotify bundle', () => {
      const result = getPlayerIcon('com.spotify.client');

      expect(result.color).toBe(colors.green);
      expect(result.svg).toBeDefined();
    });

    test('returns youtube icon for edge bundle', () => {
      const result = getPlayerIcon('com.microsoft.edgemac.Dev');

      expect(result.color).toBe(colors.red);
      expect(result.svg).toBeDefined();
    });

    test('returns default icon for unknown bundle', () => {
      const result = getPlayerIcon('com.unknown.app');

      expect(result.color).toBe(colors.text);
      expect(result.svg).toBeDefined();
    });
  });

  describe('parseMediaPayload', () => {
    test('transforms payload with artist and title when playing', () => {
      const payload: MediaPayload = {
        artist: 'Test Artist',
        title: 'Test Song',
        artwork: null,
        bundleIdentifier: 'com.spotify.client',
        playing: true,
      };

      const result = parseMediaPayload(payload);

      expect(result.prefix).toBe('');
      expect(result.label).toBe('Test Song - Test Artist');
      expect(result.bundleIdentifier).toBe('com.spotify.client');
    });

    test('transforms payload with paused prefix', () => {
      const payload: MediaPayload = {
        artist: 'Test Artist',
        title: 'Test Song',
        artwork: null,
        bundleIdentifier: 'com.apple.Music',
        playing: false,
      };

      const result = parseMediaPayload(payload);

      expect(result.prefix).toBe('Paused: ');
    });

    test('transforms payload with only title', () => {
      const payload: MediaPayload = {
        artist: '',
        title: 'Test Song',
        artwork: null,
        bundleIdentifier: 'com.test.app',
        playing: true,
      };

      const result = parseMediaPayload(payload);

      expect(result.label).toBe('Test Song');
    });
  });
});

describe('Media Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('renders nothing when no media is playing', async () => {
    invokeMock.mockResolvedValue(undefined);

    const queryClient = createTestQueryClient();
    const { container } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.innerHTML).toBe('');
    });

    queryClient.clear();
  });

  test('renders media info when playing', async () => {
    const mockPayload: MediaPayload = {
      artist: 'Test Artist',
      title: 'Test Song',
      artwork: null,
      bundleIdentifier: 'com.spotify.client',
      playing: true,
    };
    invokeMock.mockResolvedValue(mockPayload);

    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Test Song - Test Artist')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders media info with paused prefix when not playing', async () => {
    const mockPayload: MediaPayload = {
      artist: 'Test Artist',
      title: 'Test Song',
      artwork: null,
      bundleIdentifier: 'com.spotify.client',
      playing: false,
    };
    invokeMock.mockResolvedValue(mockPayload);

    const queryClient = createTestQueryClient();
    const { getByText } = await render(<Media />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Paused:')).toBeDefined();
      expect(getByText('Test Song - Test Artist')).toBeDefined();
    });

    queryClient.clear();
  });
});
