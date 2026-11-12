import { describe, expect, it } from 'vitest';

import { hostStatusFixture } from '../../../tests/fixtures';

import { alertMessage, guideSection, HEROES, isCentered } from './AccessibilityGuide.state';

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
    expect(HEROES.checking.tone).toBe('neutral');
    expect(HEROES.blocked.tone).toBe('accent');
    expect(HEROES.revoked.tone).toBe('attention');
    expect(HEROES.open).toEqual({
      tone: 'success',
      title: 'Stache Next',
      body: 'Access granted. Starting Stache Next…',
    });
  });

  it('joins the host error and command failures', () => {
    expect(alertMessage(null, [])).toBe('');
    expect(alertMessage('gate failed', [])).toBe('gate failed');
    expect(alertMessage(undefined, ['denied', 'refused'])).toBe('denied; refused');
    expect(alertMessage('gate failed', ['denied'])).toBe('gate failed; denied');
  });
});
