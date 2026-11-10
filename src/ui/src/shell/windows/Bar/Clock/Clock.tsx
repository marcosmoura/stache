import { Time03Icon } from '@hugeicons/core-free-icons';

import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { Surface } from '../../../components/Surface';
import { barClick } from '../Bar.actions';

import { useClock } from './Clock.state';

export const Clock = () => {
  const { clock } = useClock();

  if (!clock) {
    return null;
  }

  return (
    <Surface as={Button} onClick={barClick('clock')}>
      <Icon icon={Time03Icon} />
      <span>{clock}</span>
    </Surface>
  );
};
