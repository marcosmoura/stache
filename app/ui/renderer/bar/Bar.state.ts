import { useTauriEventQuery, useDisableRightClick } from '@/hooks';
import { MenubarEvents } from '@/types';

export const useBar = () => {
  const { data: menuHidden } = useTauriEventQuery<boolean>({
    eventName: MenubarEvents.VISIBILITY_CHANGED,
    queryOptions: {
      refetchOnMount: true,
    },
  });

  useDisableRightClick();

  return { menuHidden };
};
