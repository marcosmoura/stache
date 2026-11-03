import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import { useCpu } from './Cpu.state';

export const Cpu = () => {
  const { temperature, usage, color, icon, onCpuClick } = useCpu();

  return (
    <Surface as={Button} onClick={onCpuClick}>
      <Icon icon={icon} color={color} />
      <span>{usage}%</span>
      {temperature && <span>{temperature}°C</span>}
    </Surface>
  );
};
