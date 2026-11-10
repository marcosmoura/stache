import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { Surface } from '../../../components/Surface';
import { barClick } from '../Bar.actions';

import { useCpu } from './Cpu.state';

export const Cpu = () => {
  const { isResolved, temperature, usage, color, icon } = useCpu();

  if (!isResolved) {
    return null;
  }

  return (
    <Surface as={Button} onClick={barClick('cpu')}>
      <Icon icon={icon} color={color} />
      <span>{usage}%</span>
      {temperature && <span>{temperature}°C</span>}
    </Surface>
  );
};
