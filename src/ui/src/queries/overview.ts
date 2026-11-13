import { mutationOptions, useMutation } from '@tanstack/react-query';

import {
  commands,
  type ModuleEnableOutcome,
  type SessionAction,
  type SessionActionOutcome,
} from '@/ipc';

/*
 * The popover Overview's commands as mutations. Retry and the two window
 * commands return at once; their effect arrives through `status-changed`.
 * Enable and Pause/Resume resolve with their typed outcome once the host
 * runner processed them. The Accessibility request, the settings pane and Quit reuse
 * `queries/access`.
 */

export const overviewMutationKeys = {
  enable: ['overview', 'enable'] as const,
  session: ['overview', 'session'] as const,
  retry: ['overview', 'retry'] as const,
  testWindow: ['overview', 'testWindow'] as const,
  guide: ['overview', 'guide'] as const,
};

export type EnableVariables = { id: string; enabled: boolean };
export type SessionVariables = { id: string; action: SessionAction };

/** `set_module_enabled(id, enabled)`: the configuration-backed enabled flag. */
export function setModuleEnabledOptions() {
  return mutationOptions({
    mutationKey: overviewMutationKeys.enable,
    mutationFn: ({ id, enabled }: EnableVariables): Promise<ModuleEnableOutcome> =>
      commands.setModuleEnabled(id, enabled),
  });
}

/** `module_session_action(id, action)`: Pause or Resume for this session. */
export function moduleSessionActionOptions() {
  return mutationOptions({
    mutationKey: overviewMutationKeys.session,
    mutationFn: ({ id, action }: SessionVariables): Promise<SessionActionOutcome> =>
      commands.moduleSessionAction(id, action),
  });
}

/** `retry_module(id)`: retries an eligible failed or unavailable module. */
export function retryModuleOptions() {
  return mutationOptions({
    mutationKey: overviewMutationKeys.retry,
    mutationFn: (id: string): Promise<void> => commands.retryModule(id),
  });
}

/** `open_test_window()`: opens or focuses the test window; the popover hides. */
export function openTestWindowOptions() {
  return mutationOptions({
    mutationKey: overviewMutationKeys.testWindow,
    mutationFn: (): Promise<void> => commands.openTestWindow(),
  });
}

/** `open_accessibility_guide()`: opens or focuses the guide; the popover hides. */
export function openAccessibilityGuideOptions() {
  return mutationOptions({
    mutationKey: overviewMutationKeys.guide,
    mutationFn: (): Promise<void> => commands.openAccessibilityGuide(),
  });
}

export function useOpenTestWindow() {
  return useMutation(openTestWindowOptions());
}

export function useOpenAccessibilityGuide() {
  return useMutation(openAccessibilityGuideOptions());
}
