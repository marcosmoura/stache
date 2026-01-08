import { Coffee02Icon } from '@hugeicons/core-free-icons';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { colors } from '@/design-system';

import { useKeepAwake } from './KeepAwake.state';
import * as styles from './KeepAwake.styles';

export const KeepAwake = () => {
  const { isSystemAwake, onKeepAwakeClick } = useKeepAwake();

  if (isSystemAwake === undefined) {
    return null;
  }

  return (
    <Surface
      as={Button}
      onClick={onKeepAwakeClick}
      data-test-state={isSystemAwake ? 'awake' : 'sleep'}
    >
      <Icon
        className={styles.icon}
        icon={Coffee02Icon}
        fill={isSystemAwake ? colors.green : 'transparent'}
        color={isSystemAwake ? colors.green : undefined}
      />
    </Surface>
  );
};
