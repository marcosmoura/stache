import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { Surface } from '@/components/Surface';

import { useWeather } from './Weather.state';
import * as styles from './Weather.styles';

export const Weather = () => {
  const { label, icon, ref, onClick } = useWeather();

  return (
    <Surface as={Button} onClick={onClick} ref={ref}>
      <Icon icon={icon} />
      <ScrollingLabel className={styles.label}>{label}</ScrollingLabel>
    </Surface>
  );
};
