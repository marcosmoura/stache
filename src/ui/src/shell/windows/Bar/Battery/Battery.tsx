import type { ReactNode } from 'react';

import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { Surface } from '../../../components/Surface';
import { barClick } from '../Bar.actions';

import { useBattery } from './Battery.state';

export function Battery(): ReactNode {
  const { percentage, label, icon, color } = useBattery();

  if (percentage == null) {
    return null;
  }

  return (
    <Surface as={Button} onClick={barClick('battery')}>
      <Icon icon={icon} color={color} />
      <span>{label}</span>
    </Surface>
  );
}
