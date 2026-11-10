import type { ReactNode } from 'react';

import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { ScrollingLabel } from '../../../components/ScrollingLabel';
import { Surface } from '../../../components/Surface';
import { barClick } from '../Bar.actions';

import { useWeather } from './Weather.state';
import * as styles from './Weather.styles';

export function Weather(): ReactNode {
  const { label, icon, isConfigured } = useWeather();

  if (!isConfigured) {
    return null;
  }

  return (
    <Surface as={Button} onClick={barClick('weather')}>
      <Icon icon={icon} />
      <ScrollingLabel className={styles.label}>{label}</ScrollingLabel>
    </Surface>
  );
}
