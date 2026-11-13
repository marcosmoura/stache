import { describe, expect, it } from 'vitest';

import { hostStatusFixture } from '../../../tests/fixtures';

import { alertMessage, guideSection, heroes, isCentered } from './AccessibilityGuide.state';

const access = (state: 'checking' | 'blocked' | 'open', revoked = false) =>
  hostStatusFixture({ access: { state, revoked, appPath: null } });

describe('guide state', () => {
  it('derives the section from the access gate', () => {
    expect(guideSection(undefined)).toBe('checking');
    expect(guideSection(access('checking'))).toBe('checking');
    expect(guideSection(access('blocked'))).toBe('blocked');
    expect(guideSection(access('blocked', true))).toBe('revoked');
    expect(guideSection(access('open'))).toBe('open');
    // `revoked` only matters while blocked.
    expect(guideSection(access('open', true))).toBe('open');
  });

  it('centers checking and open only', () => {
    expect(isCentered('checking')).toBe(true);
    expect(isCentered('open')).toBe(true);
    expect(isCentered('blocked')).toBe(false);
    expect(isCentered('revoked')).toBe(false);
  });

  it('gives each section its tone and text', () => {
    expect(heroes('Stache').checking.tone).toBe('neutral');
    expect(heroes('Stache').blocked.tone).toBe('accent');
    expect(heroes('Stache').revoked.tone).toBe('attention');
    expect(heroes('Stache').open).toEqual({
      tone: 'success',
      title: 'Stache',
      body: 'Access granted. Starting Stache…',
    });
  });

  it('names the running build in every hero', () => {
    const dev = heroes('Stache Next');

    expect(dev.blocked.title).toBe('Stache Next needs Accessibility access');
    expect(dev.revoked.body).toBe('Stache Next stopped all features until it is turned on again.');
    expect(dev.open.body).toBe('Access granted. Starting Stache Next…');
  });

  it('joins the host error and command failures', () => {
    expect(alertMessage(null, [])).toBe('');
    expect(alertMessage('gate failed', [])).toBe('gate failed');
    expect(alertMessage(undefined, ['denied', 'refused'])).toBe('denied; refused');
    expect(alertMessage('gate failed', ['denied'])).toBe('gate failed; denied');
  });
});
