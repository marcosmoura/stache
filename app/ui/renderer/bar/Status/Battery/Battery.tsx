import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import { useBattery } from './Battery.state';

export const Battery = () => {
  const { onClick, percentage, label, icon, color, ref } = useBattery();

  if (!percentage) {
    return null;
  }

  return (
    <Surface as={Button} onClick={onClick} ref={ref}>
      <Icon icon={icon} color={color} />
      <span>{label}</span>
    </Surface>
  );
};
