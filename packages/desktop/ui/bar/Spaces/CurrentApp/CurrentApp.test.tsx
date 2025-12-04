import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { createQueryClientWrapper, createTestQueryClient } from '@/tests/utils';

import { CurrentApp } from './CurrentApp';

import { fetchCurrentHyprspaceWindow, getAppIcon, onCLIEvent } from './CurrentApp.service';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const invokeMock = vi.mocked(invoke);

describe('CurrentApp Service', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  describe('fetchCurrentHyprspaceWindow', () => {
    test('invokes get_hyprspace_focused_window', async () => {
      const mockWindow = [{ appName: 'Code', title: 'test.ts - Project' }];
      invokeMock.mockResolvedValue(mockWindow);

      const result = await fetchCurrentHyprspaceWindow();

      expect(invokeMock).toHaveBeenCalledWith('get_hyprspace_focused_window');
      expect(result).toEqual(mockWindow);
    });
  });

  describe('getAppIcon', () => {
    test('returns correct icon for Code', () => {
      const icon = getAppIcon('Code');

      expect(icon).toBeDefined();
    });

    test('returns correct icon for Finder', () => {
      const icon = getAppIcon('Finder');

      expect(icon).toBeDefined();
    });

    test('returns correct icon for Spotify', () => {
      const icon = getAppIcon('Spotify');

      expect(icon).toBeDefined();
    });

    test('returns default icon for unknown app', () => {
      const icon = getAppIcon('Unknown App');

      expect(icon).toBeDefined();
    });

    test('trims whitespace from app name', () => {
      const icon = getAppIcon('  Code  ');

      expect(icon).toBeDefined();
    });
  });

  describe('onCLIEvent', () => {
    test('refetches when focus-changed event is received', () => {
      const queryClient = createTestQueryClient();
      const refetchSpy = vi.spyOn(queryClient, 'refetchQueries');

      onCLIEvent({ name: 'focus-changed' }, queryClient);

      expect(refetchSpy).toHaveBeenCalledWith({ queryKey: ['hyprspace_focused_window'] });

      queryClient.clear();
    });

    test('does nothing for other events', () => {
      const queryClient = createTestQueryClient();
      const refetchSpy = vi.spyOn(queryClient, 'refetchQueries');

      onCLIEvent({ name: 'other-event' }, queryClient);

      expect(refetchSpy).not.toHaveBeenCalled();

      queryClient.clear();
    });
  });
});

describe('CurrentApp Component', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('renders current app when window data is available', async () => {
    invokeMock.mockResolvedValue([{ appName: 'Code', title: 'test.ts - My Project' }]);

    const queryClient = createTestQueryClient();
    const { getByText } = await render(<CurrentApp />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(getByText('Code')).toBeDefined();
    });

    queryClient.clear();
  });

  test('renders nothing when no window data', async () => {
    invokeMock.mockResolvedValue([]);

    const queryClient = createTestQueryClient();
    const { container } = await render(<CurrentApp />, {
      wrapper: createQueryClientWrapper(queryClient),
    });

    await vi.waitFor(() => {
      expect(container.innerHTML).toBe('');
    });

    queryClient.clear();
  });
});
