import { Time03Icon } from '@hugeicons/core-free-icons';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import { useClock } from './Clock.state';

export const Clock = () => {
  const { clock, onClick, ref } = useClock();

  if (!clock) {
    return null;
  }

  return (
    <Surface as={Button} onClick={onClick} ref={ref}>
      <Icon icon={Time03Icon} />
      <span>{clock}</span>
    </Surface>
  );
};
