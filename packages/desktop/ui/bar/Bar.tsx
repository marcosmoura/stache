import { cx } from '@linaria/core';

import { useDisableRightClick, useTauriEventQuery } from '@/hooks';

import { Media } from './Media';
import { Spaces } from './Spaces';
import { Status } from './Status';

import * as styles from './Bar.styles';

export const Bar = () => {
  const { data: menuHidden } = useTauriEventQuery<boolean>({
    eventName: 'tauri_menubar_visibility_changed',
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
