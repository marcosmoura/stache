import { mockWindows } from '@tauri-apps/api/mocks';
import { describe, expect, it } from 'vitest';

import { mockHost } from '../../tests/ipc';

import { closeCurrentWindow, dragRegion } from './window';

describe('window wrappers', () => {
  it('closes the current window by its label', async () => {
    const host = mockHost().answer('plugin:window|close', null);

    mockWindows('stache-access-1');
    await closeCurrentWindow();
    expect(host.callsOf('plugin:window|close')).toEqual([
      { command: 'plugin:window|close', args: { label: 'stache-access-1' } },
    ]);
  });

  it('never throws when the close is refused', async () => {
    mockHost();
    mockWindows('stache-access-1');
    await expect(closeCurrentWindow()).resolves.toBeUndefined();
  });

  it('marks a drag region with the Tauri attribute', () => {
    expect(dragRegion).toEqual({ 'data-tauri-drag-region': true });
  });
});
