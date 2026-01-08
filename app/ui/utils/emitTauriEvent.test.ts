import { beforeEach, describe, expect, it, vi } from 'vitest';

import { emitTauriEvent } from './emitTauriEvent';

vi.mock('@tauri-apps/api/event', () => ({
  emitTo: vi.fn(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => ({ label: 'test-window' })),
}));

describe('emitTauriEvent', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('should emit to current window when no target is specified', async () => {
    const { emitTo } = await import('@tauri-apps/api/event');
    const { getCurrentWindow } = await import('@tauri-apps/api/window');

    emitTauriEvent({ eventName: 'test-event' });

    expect(getCurrentWindow).toHaveBeenCalled();
    expect(emitTo).toHaveBeenCalledWith('test-window', 'test-event', undefined);
  });

  it('should emit to specified target when provided', async () => {
    const { emitTo } = await import('@tauri-apps/api/event');
    const { getCurrentWindow } = await import('@tauri-apps/api/window');

    emitTauriEvent({ eventName: 'test-event', target: 'other-window' });

    expect(getCurrentWindow).not.toHaveBeenCalled();
    expect(emitTo).toHaveBeenCalledWith('other-window', 'test-event', undefined);
  });

  it('should include payload when emitting to current window', async () => {
    const { emitTo } = await import('@tauri-apps/api/event');

    const payload = { foo: 'bar', count: 42 };
    emitTauriEvent({ eventName: 'test-event', payload });

    expect(emitTo).toHaveBeenCalledWith('test-window', 'test-event', payload);
  });

  it('should include payload when emitting to specified target', async () => {
    const { emitTo } = await import('@tauri-apps/api/event');

    const payload = { data: [1, 2, 3] };
    emitTauriEvent({ eventName: 'custom-event', target: 'target-window', payload });

    expect(emitTo).toHaveBeenCalledWith('target-window', 'custom-event', payload);
  });

  it('should handle empty object payload', async () => {
    const { emitTo } = await import('@tauri-apps/api/event');

    emitTauriEvent({ eventName: 'event', payload: {} });

    expect(emitTo).toHaveBeenCalledWith('test-window', 'event', {});
  });

  it('should handle complex nested payload', async () => {
    const { emitTo } = await import('@tauri-apps/api/event');

    const payload = {
      user: { name: 'test', id: 123 },
      items: [{ a: 1 }, { b: 2 }],
      nested: { deep: { value: true } },
    };
    emitTauriEvent({ eventName: 'complex-event', target: 'widgets', payload });

    expect(emitTo).toHaveBeenCalledWith('widgets', 'complex-event', payload);
  });
});
