import { mutationOptions, useMutation } from '@tanstack/react-query';

import { commands } from '@/ipc';

/*
 * The Accessibility guide's commands as mutations. Each returns at once; its
 * effect arrives through `status-changed`. Only a rejected `invoke` is an
 * error, which the guide shows. The guide disables a button while its
 * mutation is pending, so a command is sent once per click.
 */

export const accessMutationKeys = {
  request: ['access', 'request'] as const,
  settings: ['access', 'settings'] as const,
  quit: ['access', 'quit'] as const,
};

/** `request_accessibility()`: asks macOS for Accessibility (a check while already open). */
export function requestAccessibilityOptions() {
  return mutationOptions({
    mutationKey: accessMutationKeys.request,
    mutationFn: (): Promise<void> => commands.requestAccessibility(),
  });
}

/** `open_accessibility_settings()`: opens the Accessibility pane of System Settings. */
export function openAccessibilitySettingsOptions() {
  return mutationOptions({
    mutationKey: accessMutationKeys.settings,
    mutationFn: (): Promise<void> => commands.openAccessibilitySettings(),
  });
}

/** `quit_app()`: quits through the shared shutdown coordinator. */
export function quitAppOptions() {
  return mutationOptions({
    mutationKey: accessMutationKeys.quit,
    mutationFn: (): Promise<void> => commands.quitApp(),
  });
}

export function useRequestAccessibility() {
  return useMutation(requestAccessibilityOptions());
}

export function useOpenAccessibilitySettings() {
  return useMutation(openAccessibilitySettingsOptions());
}

export function useQuitApp() {
  return useMutation(quitAppOptions());
}
