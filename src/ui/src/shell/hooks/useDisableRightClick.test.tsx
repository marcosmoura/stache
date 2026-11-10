import { afterEach, describe, expect, test } from 'vitest';

import {
  attachContextMenuListener,
  resetDisableRightClickForTesting,
  setDisableRightClickDevModeForTesting,
} from './useDisableRightClick';

afterEach(() => {
  resetDisableRightClickForTesting();
  setDisableRightClickDevModeForTesting(null);
});

describe('useDisableRightClick', () => {
  test('blocks the context menu in a production build and releases it on cleanup', () => {
    setDisableRightClickDevModeForTesting(false);

    const detach = attachContextMenuListener();
    const blocked = new MouseEvent('contextmenu', { cancelable: true });

    document.dispatchEvent(blocked);
    expect(blocked.defaultPrevented).toBe(true);

    detach();

    const allowed = new MouseEvent('contextmenu', { cancelable: true });

    document.dispatchEvent(allowed);
    expect(allowed.defaultPrevented).toBe(false);
  });

  test('leaves the context menu alone in development', () => {
    setDisableRightClickDevModeForTesting(true);
    attachContextMenuListener();

    const event = new MouseEvent('contextmenu', { cancelable: true });

    document.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
  });
});
