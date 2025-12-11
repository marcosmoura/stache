import { cx } from '@linaria/core';

import { useDisableRightClick, useTauriEventQuery } from '@/hooks';
import { MenubarEvents } from '@/types';

import { Media } from './Media';
import { Spaces } from './Spaces';
import { Status } from './Status';

import * as styles from './Bar.styles';

export const Bar = () => {
  const { data: menuHidden } = useTauriEventQuery<boolean>({
    eventName: MenubarEvents.VISIBILITY_CHANGED,
    queryOptions: {
      refetchOnMount: true,
    },
  });

  useDisableRightClick();

  return (
    <div className={cx(styles.bar, menuHidden ? styles.barHidden : '')}>
      <Spaces />
      <Media />
      <Status />
    </div>
  );
};
