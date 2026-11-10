import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { commands, userEvent } from 'vitest/browser';

import { appInfo, hostStatus } from '../../../../tests/hostFixtures';
import { renderWithClient } from '../../../../tests/renderWithClient';
import type { HostStatus } from '../../../sdk/client';

import { AccessibilityGuide } from './AccessibilityGuide';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
  once: vi.fn(),
  emit: vi.fn(),
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

type StatusHandler = (event: { payload: { status: HostStatus } }) => void;

const APP_PATH = '/Applications/Stache Next.app';
const close = vi.fn();

function access(
  state: HostStatus['access']['state'],
  overrides: Partial<HostStatus> & { revoked?: boolean; appPath?: string | null } = {},
): HostStatus {
  const { revoked = false, appPath = APP_PATH, ...rest } = overrides;

  return hostStatus({ access: { state, revoked, appPath }, ...rest });
}

function mockStatus(status: HostStatus) {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return status;
    }

    return undefined;
  }) as never);
}

function emitStatus(status: HostStatus) {
  const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as StatusHandler | undefined;

  if (!handler) {
    throw new Error('status listener was not registered');
  }

  handler({ payload: { status } });
}

function calls(command: string) {
  return vi.mocked(invoke).mock.calls.filter(([name]) => name === command);
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  close.mockReset();
  close.mockResolvedValue(undefined);
  vi.mocked(getCurrentWindow).mockReset();
  vi.mocked(getCurrentWindow).mockReturnValue({ close } as never);
  vi.mocked(listen).mockResolvedValue(() => {});
  mockStatus(access('blocked'));
});

test('shows a short loading line until the host reports a status', async () => {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    return new Promise(() => {});
  }) as never);

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByText('Checking Accessibility access…')).toBeVisible();
  expect(close).not.toHaveBeenCalled();
});

test('shows the checking state reported by the host', async () => {
  mockStatus(access('checking'));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByText('Checking Accessibility access…')).toBeVisible();
  expect(document.body.textContent).not.toContain('Troubleshooting');
});

test('blocked shows the guidance copy, identity and collapsed troubleshooting', async () => {
  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('heading', { name: 'Stache Next needs Accessibility access' }))
    .toBeVisible();
  const text = document.body.textContent ?? '';

  expect(text).toContain(
    'Stache Next reads keyboard shortcuts and controls windows and apps. macOS only allows that with Accessibility access. Until you grant it, Stache Next stays idle.',
  );
  expect(text).toContain('Click Request Access, or Open System Settings.');
  expect(text).toContain('In Privacy & Security > Accessibility, turn on Stache Next.');
  expect(text).toContain('Come back here; Stache Next starts by itself.');
  expect(text).toContain(appInfo.identifier);
  expect(text).toContain(APP_PATH);
  expect(text).toContain('Make sure you turn on Stache Next, not Stache.');
  expect(text).toContain('The entry belongs to an older build.');
  expect(text).not.toContain('This copy is not an app bundle.');

  const details = document.querySelector('details');

  expect(details?.open).toBe(false);
  await screen.getByText('Troubleshooting').click();
  expect(details?.open).toBe(true);
});

test('revoked uses the alternate title and line', async () => {
  mockStatus(access('blocked', { revoked: true }));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('heading', { name: 'Accessibility access was turned off' }))
    .toBeVisible();
  await expect
    .element(screen.getByText('Stache Next stopped all features until it is turned on again.'))
    .toBeVisible();
  expect(document.body.textContent).not.toContain('Until you grant it');
});

test('an unbundled copy shows the terminal troubleshooting variant', async () => {
  mockStatus(access('blocked', { appPath: null }));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('heading', { name: 'Stache Next needs Accessibility access' }))
    .toBeVisible();
  const text = document.body.textContent ?? '';

  expect(text).toContain(
    'This copy is not an app bundle. macOS may assign the permission to the app that launched it, such as your terminal.',
  );
  expect(text).not.toContain('belongs to an older build');
  expect(text).toContain('not an app bundle');
});

test('open shows the granted message and closes the window', async () => {
  mockStatus(access('open'));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByText('Access granted. Starting Stache Next…')).toBeVisible();
  await vi.waitFor(() => expect(close).toHaveBeenCalledTimes(1));
  expect(document.body.textContent).not.toContain('Request Access');
});

test('a status event that opens the gate closes a blocked window', async () => {
  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('heading', { name: 'Stache Next needs Accessibility access' }))
    .toBeVisible();
  expect(close).not.toHaveBeenCalled();

  emitStatus(access('open'));

  await expect.element(screen.getByText('Access granted. Starting Stache Next…')).toBeVisible();
  await vi.waitFor(() => expect(close).toHaveBeenCalledTimes(1));
});

test.each([
  ['Request Access', 'request_accessibility'],
  ['Open System Settings', 'open_accessibility_settings'],
  ['Quit Stache Next', 'quit_app'],
])('the %s button calls %s once', async (name, command) => {
  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('button', { name })).toBeVisible();

  await screen.getByRole('button', { name }).click();

  await vi.waitFor(() => expect(calls(command)).toHaveLength(1));

  for (const other of ['request_accessibility', 'open_accessibility_settings', 'quit_app']) {
    if (other !== command) {
      expect(calls(other)).toHaveLength(0);
    }
  }
});

test('a button is disabled while its command is in flight', async () => {
  let finish: (() => void) | undefined;

  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return access('blocked');
    }

    if (command === 'request_accessibility') {
      return new Promise<void>((resolve) => {
        finish = resolve;
      });
    }

    return undefined;
  }) as never);

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('button', { name: 'Request Access' })).toBeVisible();

  await screen.getByRole('button', { name: 'Request Access' }).click();

  await expect.element(screen.getByRole('button', { name: 'Request Access' })).toBeDisabled();
  await expect.element(screen.getByRole('button', { name: 'Open System Settings' })).toBeEnabled();
  finish?.();
  await expect.element(screen.getByRole('button', { name: 'Request Access' })).toBeEnabled();
  expect(calls('request_accessibility')).toHaveLength(1);
});

test('shows the host action error and a rejected command', async () => {
  mockStatus(access('blocked', { error: 'cannot open Accessibility settings: denied' }));
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return access('blocked', { error: 'cannot open Accessibility settings: denied' });
    }

    if (command === 'quit_app') {
      throw new Error('quit refused');
    }

    return undefined;
  }) as never);

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('alert'))
    .toHaveTextContent('cannot open Accessibility settings: denied');
  await screen.getByRole('button', { name: 'Quit Stache Next' }).click();
  await expect
    .element(screen.getByRole('alert'))
    .toHaveTextContent('cannot open Accessibility settings: denied; quit refused');
});

test('renders the error alert outside the scrolling body, above the footer', async () => {
  mockStatus(access('blocked', { error: 'cannot open Accessibility settings: denied' }));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('alert')).toBeVisible();

  const alert = document.querySelector('[role="alert"]');
  const body = document.querySelector('[data-layout]');
  const footer = document.querySelector('footer');

  expect(body).not.toBeNull();
  expect(body?.contains(alert)).toBe(false);
  expect(alert?.parentElement).toBe(panel());
  expect(alert?.nextElementSibling).toBe(footer);
});

test('centers the checking hero in the body', async () => {
  mockStatus(access('checking'));

  const checking = await renderWithClient(<AccessibilityGuide />);

  await expect.element(checking.screen.getByRole('status')).toBeVisible();
  expect(document.querySelector('[data-layout]')?.getAttribute('data-layout')).toBe('centered');
});

test('centers the open hero in the body', async () => {
  mockStatus(access('open'));

  const open = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(open.screen.getByText('Access granted. Starting Stache Next…'))
    .toBeVisible();
  expect(document.querySelector('[data-layout]')?.getAttribute('data-layout')).toBe('centered');
});

test('keeps the blocked hero at the top of the body', async () => {
  mockStatus(access('blocked'));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('heading', { level: 1 })).toBeVisible();
  expect(document.querySelector('[data-layout]')?.getAttribute('data-layout')).toBe('top');
});

function panel() {
  const element = document.querySelector<HTMLElement>('main[data-state]');

  if (!element) {
    throw new Error('panel missing');
  }

  return element;
}

function badge() {
  const element = document.querySelector<HTMLElement>('[data-tone]');

  if (!element) {
    throw new Error('badge missing');
  }

  return element;
}

afterEach(async () => {
  await commands.setReducedMotion(false);
});

test('the header is a drag region and Close sits outside it', async () => {
  const { screen } = await renderWithClient(<AccessibilityGuide />);
  const closeButton = screen.getByRole('button', { name: 'Close' });

  await expect.element(closeButton).toBeVisible();

  const region = document.querySelector('header [data-tauri-drag-region]');

  expect(region?.textContent).toContain('Stache Next');
  const element = closeButton.element();

  expect(element.hasAttribute('data-tauri-drag-region')).toBe(false);
  expect(element.closest('[data-tauri-drag-region]')).toBeNull();
  expect(document.querySelectorAll('h1')).toHaveLength(1);
});

test('Close and one Escape each call close once; a repeated Escape does not', async () => {
  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('button', { name: 'Close' })).toBeVisible();

  await screen.getByRole('button', { name: 'Close' }).click();
  expect(close).toHaveBeenCalledTimes(1);

  close.mockClear();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledTimes(1);

  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', repeat: true }));
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
  expect(close).toHaveBeenCalledTimes(1);
});

test.each([
  ['checking', {}, 'checking', 'overlay'],
  ['blocked', {}, 'blocked', 'mauve'],
  ['blocked', { revoked: true }, 'blocked', 'peach'],
  ['blocked', { appPath: null }, 'blocked', 'mauve'],
  ['open', {}, 'open', 'green'],
] as const)('%s %j sets data-state %s and tone %s', async (state, overrides, dataState, tone) => {
  mockStatus(access(state, overrides));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('main')).toHaveAttribute('data-state', dataState);
  await vi.waitFor(() => expect(badge().dataset.tone).toBe(tone));
});

test('the panel starts in the checking state before the host answers', async () => {
  vi.mocked(invoke).mockImplementation((async () => new Promise(() => {})) as never);

  await renderWithClient(<AccessibilityGuide />);

  expect(panel().dataset.state).toBe('checking');
  expect(badge().dataset.tone).toBe('overlay');
});

test('the identity card lists name, bundle id and path', async () => {
  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('heading', { name: 'Stache Next needs Accessibility access' }))
    .toBeVisible();

  const rows = [...document.querySelectorAll('dl > div')].map((row) => [
    row.querySelector('dt')?.textContent,
    row.querySelector('dd')?.textContent,
  ]);

  expect(rows).toEqual([
    ['Name', appInfo.name],
    ['Bundle ID', appInfo.identifier],
    ['Path', APP_PATH],
  ]);
});

test('an unbundled copy reads "not an app bundle" in the Path row', async () => {
  mockStatus(access('blocked', { appPath: null }));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect
    .element(screen.getByRole('heading', { name: 'Stache Next needs Accessibility access' }))
    .toBeVisible();

  const path = [...document.querySelectorAll('dl > div')].find(
    (row) => row.querySelector('dt')?.textContent === 'Path',
  );

  expect(path?.querySelector('dd')?.textContent).toBe('not an app bundle');
});

test('the actions keep their order and a pending command shows a spinner', async () => {
  let finish: (() => void) | undefined;

  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return access('blocked');
    }

    if (command === 'open_accessibility_settings') {
      return new Promise<void>((resolve) => {
        finish = resolve;
      });
    }

    return undefined;
  }) as never);

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('button', { name: 'Request Access' })).toBeVisible();

  const names = [...document.querySelectorAll('footer button')].map((b) => b.textContent);

  expect(names).toEqual(['Request Access', 'Open System Settings', 'Quit Stache Next']);
  expect(document.querySelector('[data-spinner]')).toBeNull();
  expect(document.querySelector('button[aria-busy]')).toBeNull();

  await screen.getByRole('button', { name: 'Open System Settings' }).click();

  const pending = screen.getByRole('button', { name: 'Open System Settings' });

  await expect.element(pending).toBeDisabled();
  await expect.element(pending).toHaveAttribute('aria-busy', 'true');
  expect(pending.element().querySelector('[data-spinner]')).not.toBeNull();
  finish?.();
  await expect.element(pending).toBeEnabled();
  expect(pending.element().querySelector('[data-spinner]')).toBeNull();
});

test('the access surface clears the document background and releases it on unmount', async () => {
  const opaque = document.createElement('style');

  opaque.textContent = 'html, body { background: rgb(255, 0, 0); }';
  document.head.append(opaque);

  try {
    const { screen } = await renderWithClient(<AccessibilityGuide />);

    await expect.element(screen.getByRole('main')).toBeVisible();

    expect(document.documentElement.dataset.surface).toBe('access');

    for (const element of [document.documentElement, document.body]) {
      expect(getComputedStyle(element).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    }

    await screen.unmount();
    expect(document.documentElement.dataset.surface).toBeUndefined();

    for (const element of [document.documentElement, document.body]) {
      expect(getComputedStyle(element).backgroundColor).toBe('rgb(255, 0, 0)');
    }
  } finally {
    opaque.remove();
  }
});

test('reduced motion turns off animations and transitions', async () => {
  await commands.setReducedMotion(true);

  const still = (element: Element | null) => {
    if (!element) {
      throw new Error('expected an animated element to be rendered');
    }

    const style = getComputedStyle(element);

    expect(style.animationName).toBe('none');
    expect(style.transitionDuration).toMatch(/^(0s,? ?)+$/);
  };

  mockStatus(access('checking'));

  const checking = await renderWithClient(<AccessibilityGuide />);

  await expect.element(checking.screen.getByRole('main')).toBeVisible();
  still(panel());
  still(badge());
  still(badge().querySelector('svg'));
  await checking.screen.unmount();

  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'app_info') {
      return appInfo;
    }

    if (command === 'host_status') {
      return access('blocked');
    }

    if (command === 'open_accessibility_settings') {
      return new Promise<void>(() => {});
    }

    return undefined;
  }) as never);

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await screen.getByRole('button', { name: 'Open System Settings' }).click();
  await expect
    .element(screen.getByRole('button', { name: 'Open System Settings' }))
    .toHaveAttribute('aria-busy', 'true');
  still(document.querySelector('[data-spinner]'));
  still(document.querySelector('[data-layout] > div'));
  still(document.querySelector('[data-chevron]'));

  for (const button of document.querySelectorAll('button')) {
    still(button);
  }
});

test('normal motion animates the checking badge', async () => {
  mockStatus(access('checking'));

  const { screen } = await renderWithClient(<AccessibilityGuide />);

  await expect.element(screen.getByRole('main')).toBeVisible();

  expect(getComputedStyle(panel()).animationName).not.toBe('none');
  expect(getComputedStyle(badge()).animationName).not.toBe('none');
});
