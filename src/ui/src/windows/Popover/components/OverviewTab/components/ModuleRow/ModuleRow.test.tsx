import { describe, expect, it, vi } from 'vitest';

import type { ModuleStatusView } from '@/ipc';

import { render } from '../../../../../../../tests/render';

import { ModuleRow, type ModuleRowProps } from './ModuleRow';

const MODULE: ModuleStatusView = {
  id: 'proxyAudio',
  state: 'failed',
  enabled: true,
  paused: false,
  revision: '1',
  detail: 'start failed: the device is gone',
  diagnostic: 'unknown field',
};

function props(overrides: Partial<ModuleRowProps> = {}): ModuleRowProps {
  return {
    module: MODULE,
    pending: null,
    error: null,
    hasTab: true,
    onSelect: vi.fn(),
    onToggle: vi.fn(),
    onSession: vi.fn(),
    onRetry: vi.fn(),
    ...overrides,
  };
}

const inList = (row: ModuleRowProps) => (
  <ul>
    <ModuleRow {...row} />
  </ul>
);

describe('ModuleRow', () => {
  it('shows the name, state, detail and diagnostic, and wires its controls', async () => {
    const row = props();
    const screen = await render(inList(row));

    await expect.element(screen.getByTestId('state')).toHaveTextContent('failed');
    await expect.element(screen.getByText('start failed: the device is gone')).toBeVisible();
    await expect.element(screen.getByText('configuration: unknown field')).toBeVisible();
    await screen.getByRole('button', { name: 'Proxy Audio' }).click();
    await screen.getByRole('button', { name: 'Retry Proxy Audio' }).click();
    await screen.getByRole('switch', { name: 'Enable Proxy Audio' }).click();
    expect(row.onSelect).toHaveBeenCalledOnce();
    expect(row.onRetry).toHaveBeenCalledOnce();
    expect(row.onToggle).toHaveBeenCalledOnce();
  });

  it('is busy while pending and shows an inline error', async () => {
    const screen = await render(inList(props({ pending: 'enable', error: 'Refused.' })));

    await expect.element(screen.getByRole('switch')).toBeDisabled();
    await expect.element(screen.getByRole('alert')).toHaveTextContent('Refused.');
  });

  it('shows the name as text without a file tab', async () => {
    const screen = await render(inList(props({ hasTab: false })));

    expect(screen.getByRole('button', { name: 'Proxy Audio' }).query()).toBeNull();
    await expect.element(screen.getByText('Proxy Audio')).toBeVisible();
  });
});
