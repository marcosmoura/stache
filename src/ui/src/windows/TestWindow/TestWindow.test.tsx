import { describe, expect, it } from 'vitest';

import { hostStatusFixture } from '../../../tests/fixtures';
import { mockHost } from '../../../tests/ipc';
import { render } from '../../../tests/render';

import { TestWindow } from './TestWindow';

const APP = { name: 'Stache Next', version: '0.1.0', identifier: 'com.marcosmoura.stache.next' };
const HUGE_REVISION = '18446744073709551615';

const READY = hostStatusFixture({
  modules: [
    {
      id: 'proxyAudio',
      state: 'disabled',
      enabled: false,
      paused: false,
      revision: HUGE_REVISION,
      detail: null,
      diagnostic: null,
    },
  ],
  config: [
    {
      file: 'proxyAudio.toml',
      path: '/cfg/proxyAudio.toml',
      state: 'valid',
      active: true,
      diagnostic: null,
      warnings: [],
    },
  ],
});

describe('TestWindow', () => {
  it('reads the host status while it is pending', async () => {
    mockHost()
      .answer('app_info', APP)
      .answer('host_status', () => new Promise(() => {}));
    const screen = await render(<TestWindow />);

    await expect.element(screen.getByRole('status')).toHaveTextContent('Reading host status…');
    await expect.element(screen.getByText('Host test surface')).toBeVisible();
    await expect.element(screen.getByText('com.marcosmoura.stache.next (v0.1.0)')).toBeVisible();
    expect(screen.container.querySelector('#modules-heading')).toBeNull();
  });

  it('reports an unavailable host status', async () => {
    mockHost()
      .answer('app_info', APP)
      .answer('host_status', () => Promise.reject(new Error('offline')));
    const screen = await render(<TestWindow />);

    await expect
      .element(screen.getByRole('alert'))
      .toHaveTextContent('Host status unavailableoffline');
  });

  it('reports a missing application identity', async () => {
    mockHost()
      .answer('app_info', () => Promise.reject(new Error('no identity')))
      .answer('host_status', READY);
    const screen = await render(<TestWindow />);

    await expect
      .element(screen.getByText('Application identity unavailable: no identity'))
      .toBeVisible();
    await expect
      .element(screen.getByRole('heading', { name: 'Stache Next', level: 1 }))
      .toBeVisible();
  });

  it('shows a ready host with its modules and files, revision as exact text', async () => {
    mockHost().answer('app_info', APP).answer('host_status', READY);
    const screen = await render(<TestWindow />);

    await expect.element(screen.getByText('Host ready')).toBeVisible();
    await expect
      .element(screen.getByRole('heading', { name: 'proxyAudio', exact: true }))
      .toBeVisible();
    await expect.element(screen.getByText(HUGE_REVISION)).toBeVisible();
    await expect.element(screen.getByText('proxyAudio.toml')).toBeVisible();
    expect(screen.container.textContent).toContain('Active configuration');
  });

  it('shows a starting host, its error and the empty lists', async () => {
    mockHost()
      .answer('app_info', APP)
      .answer('host_status', hostStatusFixture({ ready: false, error: 'socket busy' }));
    const screen = await render(<TestWindow />);

    await expect.element(screen.getByText('Host starting')).toBeVisible();
    await expect.element(screen.getByRole('alert')).toHaveTextContent('Host errorsocket busy');
    await expect.element(screen.getByText('No modules registered.')).toBeVisible();
    await expect.element(screen.getByText('No configuration files.')).toBeVisible();
  });

  it('follows status-changed and calls only app_info, host_status and listen', async () => {
    const host = mockHost().answer('app_info', APP).answer('host_status', hostStatusFixture());
    const screen = await render(<TestWindow />);

    await expect.element(screen.getByText('No modules registered.')).toBeVisible();
    host.emit('status-changed', { status: READY });
    await expect.element(screen.getByText(HUGE_REVISION)).toBeVisible();
    expect(new Set(host.calls.map((call) => call.command))).toEqual(
      new Set(['plugin:event|listen', 'host_status', 'app_info']),
    );
  });

  it('matches the DOM the native smoke harness reads', async () => {
    mockHost().answer('app_info', APP).answer('host_status', READY);
    const screen = await render(<TestWindow />);

    await expect.element(screen.getByText(HUGE_REVISION)).toBeVisible();
    const cards = Array.from(document.querySelectorAll('li'));
    const find = (name: string) =>
      cards.find((card) => card.querySelector('h3')?.textContent === name)!;
    const module = find('proxyAudio');
    const revisionRow = Array.from(module.querySelectorAll('div')).find(
      (row) =>
        Array.from(row.children).find((child) => child.tagName === 'DT')?.textContent ===
        'Revision',
    )!;

    expect(
      Array.from(revisionRow.children).find((child) => child.tagName === 'DD')?.textContent,
    ).toBe(HUGE_REVISION);
    expect(module.querySelector('[data-state]')?.textContent).toBe('Disabled');
    expect(find('proxyAudio.toml').querySelector('[data-state]')?.textContent).toBe('Valid');
    expect(document.body.innerText).toContain('Host ready');
  });
});
