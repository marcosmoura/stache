import { describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';

import type { HostStatus_Serialize, ModuleStatusView_Serialize } from '@/ipc';

import { hostStatusFixture } from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';
import { render } from '../../../../../tests/render';
import { Popover } from '../../Popover';

import { OverviewTab } from './OverviewTab';

const module = (
  id: string,
  overrides: Partial<ModuleStatusView_Serialize> = {},
): ModuleStatusView_Serialize => ({
  id,
  state: 'running',
  enabled: true,
  paused: false,
  revision: '1',
  detail: null,
  diagnostic: null,
  ...overrides,
});

const MODULES = [
  module('bar'),
  module('tiling'),
  module('keepAwake', { state: 'disabled', enabled: false }),
  module('proxyAudio', {
    state: 'failed',
    detail: 'start failed (io): the device is gone',
  }),
  module('notunes', { state: 'paused', paused: true }),
  module('menuAnywhere', { state: 'waitingForPermission', detail: 'waiting for input permission' }),
  module('wallpapers', { state: 'unavailable', detail: 'missing executable `x`' }),
  module('commandQuit', { diagnostic: 'unknown field `foo`' }),
];

const STATUS = hostStatusFixture({ modules: MODULES });

function setup(status: HostStatus_Serialize = STATUS) {
  return mockHost()
    .answer('host_status', status)
    .answer('set_module_enabled', { kind: 'applied' })
    .answer('retry_module', null)
    .answer('module_session_action', { kind: 'applied' })
    .answer('open_test_window', null)
    .answer('open_accessibility_guide', null)
    .answer('request_accessibility', null)
    .answer('open_accessibility_settings', null)
    .answer('quit_app', null);
}

const row = (container: HTMLElement, id: string) =>
  container.querySelector<HTMLElement>(`[data-module="${id}"]`);

describe('OverviewTab', () => {
  it('renders one row per module with name, state, controls, detail and diagnostic', async () => {
    setup();
    const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);

    await expect.element(screen.getByText('Hold to Quit')).toBeVisible();
    const ids = [...screen.container.querySelectorAll<HTMLElement>('[data-module]')].map(
      (element) => element.dataset.module,
    );

    expect(ids).toEqual(MODULES.map((entry) => entry.id));
    expect(row(screen.container, 'proxyAudio')?.textContent).toContain('failed');
    expect(row(screen.container, 'menuAnywhere')?.textContent).toContain('waiting for permission');
    const tone = (id: string) =>
      row(screen.container, id)?.querySelector<HTMLElement>('[data-testid="state"]')?.dataset.tone;

    expect(MODULES.map((entry) => [entry.id, tone(entry.id)])).toEqual([
      ['bar', 'green'],
      ['tiling', 'green'],
      ['keepAwake', 'gray'],
      ['proxyAudio', 'red'],
      ['notunes', 'orange'],
      ['menuAnywhere', 'orange'],
      ['wallpapers', 'red'],
      ['commandQuit', 'green'],
    ]);
    await expect
      .element(screen.getByText('start failed (io): the device is gone'))
      .toHaveAttribute('title', 'start failed (io): the device is gone');
    await expect.element(screen.getByText('configuration: unknown field `foo`')).toBeVisible();

    await expect
      .element(screen.getByRole('switch', { name: 'Enable Keep Awake' }))
      .not.toBeChecked();
    await expect.element(screen.getByRole('switch', { name: 'Enable Tiling' })).toBeChecked();
    await expect.element(screen.getByRole('button', { name: 'Pause Tiling' })).toBeVisible();
    await expect.element(screen.getByRole('button', { name: 'Resume noTunes' })).toBeVisible();
    await expect.element(screen.getByRole('button', { name: 'Retry Proxy Audio' })).toBeVisible();
    await expect.element(screen.getByRole('button', { name: 'Retry Wallpapers' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Pause Keep Awake' }).query()).toBeNull();
    expect(screen.getByRole('button', { name: 'Retry Tiling' }).query()).toBeNull();
    expect(screen.getByRole('region', { name: /Accessibility/ }).query()).toBeNull();
  });

  for (const scheme of ['light', 'dark'] as const) {
    it(`draws native controls in system colours (${scheme})`, async () => {
      document.documentElement.style.colorScheme = scheme;
      const resolve = (keyword: string) => {
        const probe = document.createElement('span');

        probe.style.color = keyword;
        document.body.append(probe);
        const value = getComputedStyle(probe).color;

        probe.remove();

        return value;
      };
      const style = (element: Element | null | undefined) =>
        getComputedStyle(element as HTMLElement);

      try {
        setup(
          hostStatusFixture({
            modules: MODULES,
            access: { state: 'blocked', revoked: false, appPath: null },
          }),
        );
        const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);
        const on = screen.getByRole('switch', { name: 'Enable Tiling' });

        await expect.element(on).toBeVisible();
        expect(style(on.element()).backgroundColor).toBe(resolve('-apple-system-control-accent'));
        expect(
          style(screen.getByRole('switch', { name: 'Enable Keep Awake' }).element())
            .backgroundColor,
        ).not.toBe(resolve('-apple-system-control-accent'));

        const dot = (id: string) =>
          style(row(screen.container, id)?.querySelector('[data-testid="state"] > span'));

        expect(dot('tiling').backgroundColor).toBe(resolve('-apple-system-green'));
        expect(dot('notunes').backgroundColor).toBe(resolve('-apple-system-orange'));
        expect(dot('proxyAudio').backgroundColor).toBe(resolve('-apple-system-red'));
        expect(dot('keepAwake').backgroundColor).toBe(resolve('-apple-system-gray'));
        expect(
          style(row(screen.container, 'tiling')?.querySelector('[data-testid="state"]')).color,
        ).toBe(resolve('-apple-system-secondary-label'));

        const primary = style(
          screen.getByRole('button', { name: 'Show Accessibility Help…' }).element(),
        );
        const push = style(
          screen.getByRole('button', { name: 'Request Accessibility…' }).element(),
        );

        expect(primary.backgroundColor).toBe(resolve('-apple-system-control-accent'));
        expect(push.color).toBe(resolve('-apple-system-label'));
        expect(push.borderRadius).toBe('5px');
      } finally {
        document.documentElement.style.colorScheme = '';
      }
    });
  }

  it('shows the header line and the host error', async () => {
    setup(hostStatusFixture({ ready: false, error: 'socket: address in use' }));
    const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);

    await expect.element(screen.getByRole('status')).toHaveTextContent('starting…');
    await expect.element(screen.getByRole('alert')).toHaveTextContent('socket: address in use');
  });

  it('keeps the switch pending until the host answers, whatever statuses arrive', async () => {
    let answer: (value: unknown) => void = () => {};
    const host = setup().answer(
      'set_module_enabled',
      () => new Promise((resolve) => (answer = resolve)),
    );
    const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);
    const toggle = screen.getByRole('switch', { name: 'Enable Keep Awake' });

    await toggle.click();
    expect(host.callsOf('set_module_enabled').map((call) => call.args)).toEqual([
      { id: 'keepAwake', enabled: true },
    ]);
    await expect.element(toggle).toBeDisabled();
    await expect.element(toggle).not.toBeChecked();

    // Statuses update the row but do not settle it.
    host.emit('status-changed', {
      status: hostStatusFixture({
        modules: MODULES.map((entry) => (entry.id === 'keepAwake' ? module('keepAwake') : entry)),
      }),
    });
    await expect.element(toggle).toBeChecked();
    await expect.element(toggle).toBeDisabled();

    answer({ kind: 'applied' });
    await expect.element(toggle).toBeEnabled();
    expect(row(screen.container, 'keepAwake')?.dataset.state).toBe('running');
    expect(row(screen.container, 'keepAwake')?.querySelector('[role="alert"]')).toBeNull();
  });

  for (const [label, message] of [
    ['an invalid keepAwake.toml', 'keepAwake.toml is invalid; not overwritten'],
    ['a symlinked keepAwake.toml', 'keepAwake.toml is a symlink; not overwritten'],
  ] as const) {
    it(`settles two identical consecutive refusals for ${label} and shows each inline`, async () => {
      const host = setup().answer('set_module_enabled', { kind: 'refused', message });
      const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);
      const toggle = screen.getByRole('switch', { name: 'Enable Keep Awake' });
      const alert = () =>
        row(screen.container, 'keepAwake')?.querySelector('[role="alert"]')?.textContent;

      for (const attempt of [1, 2]) {
        await toggle.click();
        await expect.element(toggle).toBeEnabled();
        await expect.element(toggle).not.toBeChecked();
        await vi.waitFor(() => expect(alert()).toBe(message));
        expect(host.callsOf('set_module_enabled')).toHaveLength(attempt);
      }
    });
  }

  it('pauses through the session action and shows a refusal inline', async () => {
    let answer: (value: unknown) => void = () => {};
    const host = setup().answer(
      'module_session_action',
      () => new Promise((resolve) => (answer = resolve)),
    );
    const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);
    const pause = screen.getByRole('button', { name: 'Pause Tiling' });

    await pause.click();
    expect(host.callsOf('module_session_action').map((call) => call.args)).toEqual([
      { module: 'tiling', action: 'pause' },
    ]);
    await expect.element(pause).toBeDisabled();
    answer({ kind: 'unavailable', reason: 'releasePending' });
    await expect.element(pause).toBeEnabled();
    expect(row(screen.container, 'tiling')?.querySelector('[role="alert"]')?.textContent).toBe(
      'Could not pause: a release is pending',
    );
  });

  it('retries until the next status and shows a rejected call inline', async () => {
    const host = setup();
    const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);
    const retry = screen.getByRole('button', { name: 'Retry Proxy Audio' });

    await retry.click();
    expect(host.callsOf('retry_module').map((call) => call.args)).toEqual([{ id: 'proxyAudio' }]);
    await expect.element(retry).toBeDisabled();
    host.emit('status-changed', { status: { ...STATUS } });
    await expect.element(retry).toBeEnabled();

    host.answer('retry_module', () => Promise.reject(new Error('not allowed')));
    await retry.click();
    await expect.element(retry).toBeEnabled();
    expect(
      row(screen.container, 'proxyAudio')?.querySelector('[role="alert"]')?.textContent,
    ).toContain('not allowed');
  });

  for (const revoked of [false, true]) {
    it(`shows the gate banner while blocked (revoked ${revoked}) and its actions call the commands`, async () => {
      const host = setup(
        hostStatusFixture({ access: { state: 'blocked', revoked, appPath: null } }),
      );
      const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);

      await expect
        .element(screen.getByRole('region', { name: /Accessibility/ }))
        .toHaveAccessibleName(revoked ? 'Accessibility was turned off' : 'Accessibility required');
      await screen.getByRole('button', { name: 'Show Accessibility Help…' }).click();
      await screen.getByRole('button', { name: 'Request Accessibility…' }).click();
      await screen.getByRole('button', { name: 'Open Accessibility Settings…' }).click();
      await vi.waitFor(() =>
        expect(
          host.calls
            .map((call) => call.command)
            .filter((command) => !command.startsWith('plugin:') && command !== 'host_status'),
        ).toEqual([
          'open_accessibility_guide',
          'request_accessibility',
          'open_accessibility_settings',
        ]),
      );
    });
  }

  for (const state of ['checking', 'open'] as const) {
    it(`shows no gate banner while ${state}`, async () => {
      setup(hostStatusFixture({ access: { state, revoked: false, appPath: null } }));
      const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);

      await expect.element(screen.getByRole('button', { name: 'Open Test Window' })).toBeVisible();
      expect(screen.getByRole('region', { name: /Accessibility/ }).query()).toBeNull();
    });
  }

  it('lists file diagnostics and warnings, and the footer actions', async () => {
    const host = setup(
      hostStatusFixture({
        config: [
          {
            file: 'tiling.toml',
            path: '/c/tiling.toml',
            state: 'invalid',
            active: false,
            diagnostic: 'parse error',
            warnings: [],
          },
          {
            file: 'bar.toml',
            path: '/c/bar.toml',
            state: 'valid',
            active: true,
            diagnostic: null,
            warnings: ['unknown key `x`'],
          },
          {
            file: 'global.toml',
            path: '/c/global.toml',
            state: 'valid',
            active: true,
            diagnostic: null,
            warnings: [],
          },
        ],
      }),
    );
    const screen = await render(<OverviewTab files={[]} onSelectTab={() => {}} />);

    await expect.element(screen.getByText('parse error')).toBeVisible();
    await expect.element(screen.getByText('unknown key `x`')).toBeVisible();
    expect(screen.container.querySelector('[data-file="global.toml"]')).toBeNull();

    await screen.getByRole('button', { name: 'Open Test Window' }).click();
    await screen.getByRole('button', { name: 'Quit Stache Next' }).click();
    await vi.waitFor(() => expect(host.callsOf('quit_app')).toHaveLength(1));
    expect(host.callsOf('open_test_window')).toHaveLength(1);
  });

  it("selects a module's file tab from its name", async () => {
    setup().answer('config_files', [
      {
        file: 'tiling.toml',
        path: '/c/tiling.toml',
        state: 'valid',
        active: true,
        diagnostic: null,
        warnings: [],
      },
    ]);
    const screen = await render(<Popover />);

    await expect.element(screen.getByRole('button', { name: 'Tiling' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Bar', exact: true }).query()).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Tiling' }));
    await expect
      .element(screen.getByRole('heading', { level: 2, name: 'tiling.toml' }))
      .toBeVisible();
    await expect
      .element(screen.getByRole('tab', { name: 'tiling.toml' }))
      .toHaveAttribute('aria-selected', 'true');
  });
});
