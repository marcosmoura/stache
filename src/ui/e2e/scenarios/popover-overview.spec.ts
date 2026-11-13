import type {
  ConfigFileView_Serialize,
  HostStatus_Serialize,
  ModuleStatusView_Serialize,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/** The Overview footer names the app from `app_info`. */
const APP = { name: 'Stache Next', version: '0.1.0', identifier: 'com.marcosmoura.stache.next' };

/*
 * The popover Overview end to end through the fake host, on a setup like the
 * user's: Bar and Tiling running, Keep Awake disabled, Proxy Audio failed.
 * Enable Keep Awake and see it turn running after the status event; pause and
 * resume Tiling; retry Proxy Audio; block the gate and use the banner; quit.
 * No switch flips before the host says so.
 */

const IDS = [
  'bar',
  'commandQuit',
  'keepAwake',
  'keybindings',
  'menuAnywhere',
  'notunes',
  'proxyAudio',
  'tiling',
  'wallpapers',
];

const module = (
  id: string,
  overrides: Partial<ModuleStatusView_Serialize> = {},
): ModuleStatusView_Serialize => ({
  id,
  state: 'disabled',
  enabled: false,
  paused: false,
  revision: '1',
  detail: null,
  diagnostic: null,
  ...overrides,
});

const file = (name: string): ConfigFileView_Serialize => ({
  file: name,
  path: `/Users/me/.config/stache/${name}`,
  state: 'valid',
  active: true,
  diagnostic: null,
  warnings: [],
});

type Modules = Record<string, Partial<ModuleStatusView_Serialize>>;

const RUNNING = { state: 'running', enabled: true } as const;

function statusOf(modules: Modules, overrides: Partial<HostStatus_Serialize> = {}) {
  return {
    ready: true,
    error: null,
    access: { state: 'open', revoked: false, appPath: null },
    modules: IDS.map((id) => module(id, modules[id])),
    config: [],
    ...overrides,
  } satisfies HostStatus_Serialize;
}

test.describe('Popover Overview', () => {
  test('enable, pause, resume, retry, the gate banner and quit', async ({
    page,
    host,
    consoleErrors,
  }) => {
    const modules: Modules = {
      bar: RUNNING,
      tiling: RUNNING,
      keepAwake: { state: 'disabled', enabled: false },
      proxyAudio: {
        state: 'failed',
        enabled: true,
        detail: 'start failed (coreAudio): the output device disappeared',
      },
    };
    let applyEnable: (outcome: unknown) => void = () => {};
    const enableAnswered = new Promise((resolve) => (applyEnable = resolve));

    host
      .answer('app_info', APP)
      .answer('config_files', ['global.toml', ...IDS.map((id) => `${id}.toml`)].map(file))
      .answerHostStatus(statusOf(modules))
      .answer('set_module_enabled', () => enableAnswered)
      .answer('retry_module', null)
      .answer('module_session_action', { kind: 'applied' })
      .answer('request_accessibility', null)
      .answer('open_accessibility_guide', null)
      .answer('quit_app', null);
    await page.goto('/?surface=popover');

    const rowOf = (id: string) => page.locator(`[data-module="${id}"]`);

    await expect(rowOf('tiling')).toHaveAttribute('data-state', 'running');
    await expect(rowOf('proxyAudio')).toContainText(
      'start failed (coreAudio): the output device disappeared',
    );

    // Enable Keep Awake: pending, unchanged, until the host's outcome.
    const keepAwake = page.getByRole('switch', { name: 'Enable Keep Awake' });

    await keepAwake.click();
    await expect
      .poll(() => host.calls.filter((c) => c.command === 'set_module_enabled'))
      .toEqual([{ command: 'set_module_enabled', args: { id: 'keepAwake', enabled: true } }]);
    await expect(keepAwake).toBeDisabled();
    await expect(keepAwake).not.toBeChecked();
    modules.keepAwake = RUNNING;
    await host.emitStatusChanged(statusOf(modules));
    await expect(keepAwake).toBeDisabled();
    applyEnable({ kind: 'applied' });
    await expect(keepAwake).toBeChecked();
    await expect(keepAwake).toBeEnabled();
    await expect(rowOf('keepAwake')).toHaveAttribute('data-state', 'running');

    // Pause Tiling, then resume it.
    await page.getByRole('button', { name: 'Pause Tiling' }).click();
    modules.tiling = { state: 'paused', enabled: true, paused: true };
    await host.emitStatusChanged(statusOf(modules));
    await expect(rowOf('tiling')).toHaveAttribute('data-state', 'paused');
    await page.getByRole('button', { name: 'Resume Tiling' }).click();
    modules.tiling = RUNNING;
    await host.emitStatusChanged(statusOf(modules));
    await expect(rowOf('tiling')).toHaveAttribute('data-state', 'running');
    await expect(page.getByRole('button', { name: 'Pause Tiling' })).toBeEnabled();
    expect(
      host.calls.filter((c) => c.command === 'module_session_action').map((c) => c.args),
    ).toEqual([
      { module: 'tiling', action: 'pause' },
      { module: 'tiling', action: 'resume' },
    ]);

    // Retry Proxy Audio.
    await page.getByRole('button', { name: 'Retry Proxy Audio' }).click();
    await expect
      .poll(() => host.calls.filter((c) => c.command === 'retry_module').map((c) => c.args))
      .toEqual([{ id: 'proxyAudio' }]);
    modules.proxyAudio = RUNNING;
    await host.emitStatusChanged(statusOf(modules));
    await expect(rowOf('proxyAudio')).toHaveAttribute('data-state', 'running');
    await expect(page.getByRole('button', { name: 'Retry Proxy Audio' })).toHaveCount(0);

    // The gate closes: the banner and its actions.
    const banner = page.getByRole('region', { name: 'Accessibility was turned off' });

    await expect(banner).toHaveCount(0);
    modules.tiling = { state: 'waitingForPermission', enabled: true };
    await host.emitStatusChanged(
      statusOf(modules, { access: { state: 'blocked', revoked: true, appPath: null } }),
    );
    await expect(banner).toBeVisible();
    await expect(rowOf('tiling')).toContainText('waiting for permission');
    await banner.getByRole('button', { name: 'Request Accessibility…' }).click();
    await banner.getByRole('button', { name: 'Show Accessibility Help…' }).click();
    await expect
      .poll(() => host.commands().filter((c) => c.includes('accessibility')))
      .toEqual(['request_accessibility', 'open_accessibility_guide']);
    modules.tiling = RUNNING;
    await host.emitStatusChanged(statusOf(modules));
    await expect(banner).toHaveCount(0);

    await page.getByRole('button', { name: 'Quit Stache Next' }).click();
    await expect.poll(() => host.commands().filter((c) => c === 'quit_app')).toHaveLength(1);

    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });
});
