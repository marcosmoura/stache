import { describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';

import type { AccessView_Serialize, HostStatus_Serialize } from '@/ipc';

import { hostStatusFixture } from '../../../tests/fixtures';
import { flush, mockCurrentWindow, mockHost, type MockHost } from '../../../tests/ipc';
import { render } from '../../../tests/render';

import { AccessibilityGuide } from './AccessibilityGuide';

const APP = { name: 'Stache', version: '0.1.0', identifier: 'com.marcosmoura.stache' };
const APP_PATH = '/Applications/Stache.app';

function status(
  access: Partial<AccessView_Serialize>,
  overrides: Partial<HostStatus_Serialize> = {},
) {
  return hostStatusFixture({
    access: { state: 'blocked', revoked: false, appPath: APP_PATH, ...access },
    ...overrides,
  });
}

function setup(initial: HostStatus_Serialize | null = status({})) {
  const host = mockHost()
    .answer('app_info', APP)
    .answer('plugin:window|close', null)
    .answer('host_status', initial === null ? () => new Promise(() => {}) : initial);

  mockCurrentWindow('stache-access-1');

  return host;
}

const closes = (host: MockHost) => host.callsOf('plugin:window|close').length;

/** A command answer the test resolves itself. */
function deferred() {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<null>((done, fail) => {
    resolve = () => done(null);
    reject = fail;
  });

  return { promise, resolve, reject };
}

describe('AccessibilityGuide', () => {
  it('shows the checking line before the first status', async () => {
    setup(null);
    const screen = await render(<AccessibilityGuide />);

    await expect
      .element(screen.getByRole('status'))
      .toHaveTextContent('Checking Accessibility access…');
    await expect.element(screen.getByRole('main')).toHaveAttribute('data-state', 'checking');
    await expect.element(screen.getByRole('button', { name: 'Request Access' })).toBeVisible();
  });

  it('shows the checking line while the gate checks', async () => {
    setup(status({ state: 'checking' }));
    const screen = await render(<AccessibilityGuide />);

    await expect.element(screen.getByRole('main')).toHaveAttribute('data-state', 'checking');
    await expect
      .element(screen.getByRole('status'))
      .toHaveTextContent('Checking Accessibility access…');
  });

  it('guides through a blocked gate with steps, identity and collapsed troubleshooting', async () => {
    setup();
    const screen = await render(<AccessibilityGuide />);

    await expect
      .element(screen.getByRole('heading', { name: 'Stache needs Accessibility access' }))
      .toBeVisible();
    await expect
      .element(screen.getByText(/Until you allow it, Stache waits and does nothing\./))
      .toBeVisible();
    expect(screen.getByRole('listitem').elements()).toHaveLength(3);
    await expect.element(screen.getByText(APP.identifier)).toBeVisible();
    await expect.element(screen.getByText(APP_PATH)).toBeVisible();
    await expect
      .element(screen.getByRole('button', { name: 'Troubleshooting' }))
      .toHaveAttribute('aria-expanded', 'false');
    await screen.getByRole('button', { name: 'Troubleshooting' }).click();
    expect(screen.container.textContent).toContain('older copy of the app');
    expect(screen.container.querySelector('[data-tone="accent"]')).not.toBeNull();
    expect(screen.container.querySelector('[role="alert"]')).toBeNull();
    // The state is a coloured dot before the title, hidden from assistive tech.
    const dot = screen.getByRole('heading').element().querySelector('[data-status-dot]')!;

    expect(dot).toHaveAttribute('aria-hidden', 'true');
    expect(getComputedStyle(dot).width).toBe('7px');
    await expect
      .element(screen.getByRole('heading'))
      .toHaveTextContent('Stache needs Accessibility access');
  });

  it('shows the unbundled variant without an app path', async () => {
    setup(status({ appPath: null }));
    const screen = await render(<AccessibilityGuide />);

    await expect.element(screen.getByText('not an app bundle')).toBeVisible();
    await screen.getByRole('button', { name: 'Troubleshooting' }).click();
    expect(screen.container.textContent).toContain("isn't installed as a regular app");
  });

  it('shows the revoked title and line', async () => {
    setup(status({ revoked: true }));
    const screen = await render(<AccessibilityGuide />);

    await expect
      .element(screen.getByRole('heading', { name: 'Accessibility access was turned off' }))
      .toBeVisible();
    await expect
      .element(screen.getByText('Stache stopped all features until it is turned on again.'))
      .toBeVisible();
    expect(screen.container.querySelector('[data-tone="attention"]')).not.toBeNull();
    await expect.element(screen.getByRole('main')).toHaveAttribute('data-state', 'revoked');
  });

  it('shows the host error when present', async () => {
    setup(status({}, { error: 'Accessibility check failed' }));
    const screen = await render(<AccessibilityGuide />);

    await expect.element(screen.getByRole('alert')).toHaveTextContent('Accessibility check failed');
  });

  it('closes itself when access opens, and hides the actions', async () => {
    const host = setup();
    const screen = await render(<AccessibilityGuide />);

    await expect.element(screen.getByRole('button', { name: 'Request Access' })).toBeVisible();
    expect(closes(host)).toBe(0);
    host.emit('status-changed', { status: status({ state: 'open' }) });
    await expect
      .element(screen.getByRole('status'))
      .toHaveTextContent('Access granted. Starting Stache…');
    await vi.waitFor(() => expect(closes(host)).toBe(1));
    expect(screen.container.querySelector('footer')).toBeNull();
    expect(screen.container.querySelector('[data-tone="success"]')).not.toBeNull();
  });

  for (const [label, command] of [
    ['Request Access', 'request_accessibility'],
    ['Open System Settings', 'open_accessibility_settings'],
    ['Quit Stache', 'quit_app'],
  ] as const) {
    it(`${label} sends ${command} once and disables while pending`, async () => {
      const host = setup();
      const answer = deferred();

      host.answer(command, () => answer.promise);
      const screen = await render(<AccessibilityGuide />);
      const button = screen.getByRole('button', { name: label });

      await button.click();
      await expect.element(button).toBeDisabled();
      (button.element() as HTMLButtonElement).click();
      await flush();
      expect(host.callsOf(command)).toHaveLength(1);
      answer.resolve();
      await expect.element(button).toBeEnabled();
      await button.click();
      await vi.waitFor(() => expect(host.callsOf(command)).toHaveLength(2));
    });
  }

  it('shows a rejected command and clears it on the next success', async () => {
    const host = setup();

    host.answer('request_accessibility', () => Promise.reject(new Error('refused')));
    const screen = await render(<AccessibilityGuide />);

    await screen.getByRole('button', { name: 'Request Access' }).click();
    await expect.element(screen.getByRole('alert')).toHaveTextContent('refused');
    host.answer('request_accessibility', null);
    await screen.getByRole('button', { name: 'Request Access' }).click();
    await vi.waitFor(() => expect(screen.container.querySelector('[role="alert"]')).toBeNull());
  });

  it('closes on Close and on Escape, ignoring key auto-repeat', async () => {
    const host = setup();
    const screen = await render(<AccessibilityGuide />);

    await screen.getByRole('button', { name: 'Close' }).click();
    await vi.waitFor(() => expect(closes(host)).toBe(1));
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', repeat: true }));
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    await flush();
    expect(closes(host)).toBe(1);
    await userEvent.keyboard('{Escape}');
    await vi.waitFor(() => expect(closes(host)).toBe(2));
  });

  it('keeps the Close button outside the drag region', async () => {
    setup();
    const screen = await render(<AccessibilityGuide />);
    const region = screen.container.querySelector('[data-tauri-drag-region]');

    expect(region).not.toBeNull();
    expect(region?.contains(screen.getByRole('button', { name: 'Close' }).element())).toBe(false);
    expect(region?.textContent).toBe('Stache');
  });

  it('names the app from app_info in the header, the copy and the Quit action', async () => {
    setup().answer('app_info', { ...APP, name: 'Injected Name' });
    const screen = await render(<AccessibilityGuide />);
    const region = screen.container.querySelector('[data-tauri-drag-region]');

    await expect.element(screen.getByRole('button', { name: 'Quit Injected Name' })).toBeVisible();
    await expect
      .element(screen.getByRole('heading', { name: 'Injected Name needs Accessibility access' }))
      .toBeVisible();
    await expect
      .element(screen.getByText(/Come back here; Injected Name starts by itself\./))
      .toBeVisible();
    await expect
      .element(
        screen.getByText('Make sure you turn on Injected Name, not another copy of the app.'),
      )
      .toBeVisible();
    expect(region?.textContent).toBe('Injected Name');
  });

  it('calls only the commands its capability allows', async () => {
    const host = setup();
    const screen = await render(<AccessibilityGuide />);

    await expect.element(screen.getByText(APP.identifier)).toBeVisible();
    expect(new Set(host.calls.map((call) => call.command))).toEqual(
      new Set(['plugin:event|listen', 'host_status', 'app_info']),
    );
  });

  it('is one rounded, transparent native panel with a scrolling body', async () => {
    setup();
    const screen = await render(<AccessibilityGuide />);
    const main = screen.getByRole('main').element();

    // The native material's radius (`ACCESS_CORNER_RADIUS`) shows through the page.
    await expect.poll(() => getComputedStyle(main).borderTopLeftRadius).toBe('12px');
    expect(getComputedStyle(main).height).toBe(`${window.innerHeight}px`);
    expect(getComputedStyle(main).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(getComputedStyle(main).fontFamily).toMatch(/^-apple-system/);
    expect(getComputedStyle(main).fontSize).toBe('13px');
    expect(getComputedStyle(document.body).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(getComputedStyle(main.children[1]!).overflowY).toBe('auto');
  });
});
