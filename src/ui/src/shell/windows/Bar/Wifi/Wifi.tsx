import type { ReactNode } from 'react';

import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { ScrollingLabel } from '../../../components/ScrollingLabel';
import { Surface } from '../../../components/Surface';
import { barClick } from '../Bar.actions';

import { useWifi } from './Wifi.state';
import * as styles from './Wifi.styles';

export function Wifi(): ReactNode {
  const { status, label, icon, color } = useWifi();

  if (status === 'Unknown') {
    return null;
  }

  return (
    <Surface as={Button} onClick={barClick('wifi')}>
      <Icon icon={icon} color={color} />
      {label ? <ScrollingLabel className={styles.label}>{label}</ScrollingLabel> : null}
    </Surface>
  );
}
