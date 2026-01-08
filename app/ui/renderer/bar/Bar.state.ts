import { useDisableRightClick } from '@/hooks';
import { useTauri } from '@/hooks/useTauri';
import { MenubarEvents } from '@/types';

export const useBar = () => {
  const { data: menuHidden } = useTauri<boolean>({
    queryKey: ['menubar-visibility'],
    queryFn: async () => false, // Default to visible
    eventName: MenubarEvents.VISIBILITY_CHANGED,
    staleTime: Infinity, // Don't refetch - updates come from events
  });

  useDisableRightClick();

  return { menuHidden };
};
