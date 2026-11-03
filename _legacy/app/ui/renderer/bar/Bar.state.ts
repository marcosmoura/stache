import { useDisableRightClick, useTauri } from '@/hooks';
import { MenubarEvents } from '@/types';

import type { BarState } from './Bar.types';

export function useBar(): BarState {
  const { data: menuHidden } = useTauri<boolean>({
    queryKey: ['menubar-visibility'],
    queryFn: async () => false,
    eventName: MenubarEvents.VISIBILITY_CHANGED,
    staleTime: Infinity,
  });

  useDisableRightClick();

  return { menuHidden };
}
