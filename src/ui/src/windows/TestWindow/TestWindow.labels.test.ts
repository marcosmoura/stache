import { describe, expect, it } from 'vitest';

import { badgeTone, CONFIG_STATE_LABELS, MODULE_STATE_LABELS, yesNo } from './TestWindow.labels';

describe('test window labels', () => {
  it('labels every module and file state distinctly', () => {
    const modules = Object.values(MODULE_STATE_LABELS);
    const files = Object.values(CONFIG_STATE_LABELS);

    expect(new Set(modules).size).toBe(modules.length);
    expect(new Set(files).size).toBe(files.length);
    expect(MODULE_STATE_LABELS.waitingForPermission).toBe('Waiting for permission');
  });

  it('tones each state', () => {
    expect(badgeTone('running')).toBe('success');
    expect(badgeTone('failed')).toBe('danger');
    expect(badgeTone('invalid')).toBe('danger');
    expect(badgeTone('waitingForPermission')).toBe('warning');
    expect(badgeTone('missing')).toBe('warning');
    expect(badgeTone('paused')).toBe('attention');
    expect(badgeTone('unavailable')).toBe('attention');
    expect(badgeTone('disabled')).toBe('neutral');
    expect(badgeTone('stopped')).toBe('neutral');
    expect(badgeTone('valid')).toBe('neutral');
  });

  it('writes flags as words', () => {
    expect(yesNo(true)).toBe('Yes');
    expect(yesNo(false)).toBe('No');
  });
});
