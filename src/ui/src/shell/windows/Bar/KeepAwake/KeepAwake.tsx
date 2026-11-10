import { Coffee02Icon } from '@hugeicons/core-free-icons';

import { colors } from '../../../../sdk/tokens';
import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { Surface } from '../../../components/Surface';
import { keepAwakeClick } from '../Bar.actions';

import { useKeepAwake } from './KeepAwake.state';
import * as styles from './KeepAwake.styles';

export const KeepAwake = () => {
  const { isSystemAwake, session } = useKeepAwake();

  if (isSystemAwake === undefined) {
    return null;
  }

  return (
    <Surface
      as={Button}
      data-test-state={isSystemAwake ? 'awake' : 'sleep'}
      onClick={keepAwakeClick(session)}
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
