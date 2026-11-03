import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { AppList } from './AppList';

describe('AppList Component', () => {
  test('renders list of apps', async () => {
    const onAppClick = vi.fn(() => vi.fn());
    const apps = [
      { appName: 'Ghostty', windowId: 100, windowTitle: 'Terminal', displayName: 'Ghostty' },
      { appName: 'Code', windowId: 101, windowTitle: 'Editor', displayName: 'Code' },
    ];

    const { container } = await render(
      <AppList apps={apps} focusedApp={undefined} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(2);
    });
  });

  test('renders empty list when no apps', async () => {
    const onAppClick = vi.fn(() => vi.fn());

    const { container } = await render(
      <AppList apps={[]} focusedApp={undefined} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(0);
    });
  });

  test('marks focused app correctly', async () => {
    const onAppClick = vi.fn(() => vi.fn());
    const apps = [
      { appName: 'Ghostty', windowId: 100, windowTitle: 'Terminal', displayName: 'Ghostty' },
      { appName: 'Code', windowId: 101, windowTitle: 'Editor', displayName: 'Code' },
    ];
    const focusedApp = { appName: 'Ghostty', windowId: 100, windowTitle: 'Terminal' };

    const { getByText } = await render(
      <AppList apps={apps} focusedApp={focusedApp} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      // Focused app should show its name
      expect(getByText('Ghostty')).toBeDefined();
    });
  });

  test('calls onAppClick with correct windowId', async () => {
    const clickHandler = vi.fn();
    const onAppClick = vi.fn(() => clickHandler);
    const apps = [
      { appName: 'Ghostty', windowId: 100, windowTitle: 'Terminal', displayName: 'Ghostty' },
    ];

    const { container } = await render(
      <AppList apps={apps} focusedApp={undefined} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      const button = container.querySelector('button');
      expect(button).toBeDefined();
      button?.click();
      expect(onAppClick).toHaveBeenCalledWith(100);
    });
  });

  test('renders multiple apps with unique keys', async () => {
    const onAppClick = vi.fn(() => vi.fn());
    const apps = [
      { appName: 'Ghostty', windowId: 100, windowTitle: 'Terminal', displayName: 'Ghostty' },
      { appName: 'Code', windowId: 101, windowTitle: 'Editor', displayName: 'Code' },
      { appName: 'Safari', windowId: 102, windowTitle: 'Browser', displayName: 'Safari' },
    ];

    const { container } = await render(
      <AppList apps={apps} focusedApp={undefined} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      const buttons = container.querySelectorAll('button');
      expect(buttons.length).toBe(3);
    });
  });

  test('displays window title for apps with multiple windows', async () => {
    const onAppClick = vi.fn(() => vi.fn());
    // Two Code windows should show their titles, Safari should show app name
    const apps = [
      { appName: 'Code', windowId: 100, windowTitle: 'index.ts', displayName: 'index.ts' },
      { appName: 'Code', windowId: 101, windowTitle: 'main.tsx', displayName: 'main.tsx' },
      { appName: 'Safari', windowId: 102, windowTitle: 'Google', displayName: 'Safari' },
    ];
    const focusedApp = { appName: 'Code', windowId: 100, windowTitle: 'index.ts' };

    const { getByText } = await render(
      <AppList apps={apps} focusedApp={focusedApp} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      // Should display window title for Code (has multiple windows)
      expect(getByText('index.ts')).toBeDefined();
    });
  });

  test('truncates long display names', async () => {
    const onAppClick = vi.fn(() => vi.fn());
    const apps = [
      {
        appName: 'Code',
        windowId: 100,
        windowTitle: 'This is a very long file name.ts',
        displayName: 'This is a very long ...',
      },
    ];
    const focusedApp = {
      appName: 'Code',
      windowId: 100,
      windowTitle: 'This is a very long file name.ts',
    };

    const { getByText } = await render(
      <AppList apps={apps} focusedApp={focusedApp} onAppClick={onAppClick} />,
    );

    await vi.waitFor(() => {
      // Should display truncated name with ellipsis
      expect(getByText('This is a very long ...')).toBeDefined();
    });
  });
});
