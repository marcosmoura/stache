import '@/main.css';
import { clearMocks } from '@tauri-apps/api/mocks';
import { MotionGlobalConfig } from 'motion/react';
import { afterEach, beforeEach } from 'vitest';
import { cleanup } from 'vitest-browser-react';
import { commands } from 'vitest/browser';

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
  await commands.emulateMedia({ colorScheme: null, reducedMotion: null });
});
