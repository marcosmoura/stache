import '@/main.css';
import { clearMocks } from '@tauri-apps/api/mocks';
import { MotionGlobalConfig } from 'motion/react';
import { afterEach, beforeEach } from 'vitest';
import { cleanup } from 'vitest-browser-react';
import { commands } from 'vitest/browser';

/*
 * `prefers-reduced-motion` is pinned to `no-preference` so the machine's
 * Reduce Motion setting never leaks in. It is set before any test of the file
 * renders, because Motion reads the query once, at its first motion component;
 * a test that wants reduced motion sets it itself and the reset restores this.
 */
await commands.emulateMedia({ reducedMotion: 'no-preference' });

/*
 * Animations are skipped by default so assertions never wait on timing; a test
 * that needs real animation sets `MotionGlobalConfig.skipAnimations = false`.
 */
beforeEach(() => {
  MotionGlobalConfig.skipAnimations = true;
});

afterEach(async () => {
  await cleanup();
  clearMocks();
  await commands.emulateMedia({ colorScheme: null, reducedMotion: 'no-preference' });
});
