import { mutationOptions, queryOptions, useMutation } from '@tanstack/react-query';

import { commands, type ConfigFileView } from '@/ipc';

/** Every catalog file in catalog order (`config_files()`); refetched on host events. */
export function configFilesOptions() {
  return queryOptions({
    queryKey: ['config', 'files'],
    queryFn: (): Promise<ConfigFileView[]> => commands.configFiles(),
  });
}

/** `hide_popover()`: hides the popover; returns at once. */
export function hidePopoverOptions() {
  return mutationOptions({
    mutationKey: ['popover', 'hide'],
    mutationFn: (): Promise<void> => commands.hidePopover(),
  });
}

export function useHidePopover() {
  return useMutation(hidePopoverOptions());
}
