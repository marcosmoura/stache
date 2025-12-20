import { Stack } from '@/components/Stack';

import { Battery } from './Battery';
import { Clock } from './Clock';
import { Cpu } from './Cpu';
import { KeepAwake } from './KeepAwake';
import { Weather } from './Weather';

import * as styles from './Status.styles';

export const Status = () => {
  return (
    <Stack className={styles.status} data-test-id="status-container">
      <Weather />
      <KeepAwake />
      <Cpu />
      <Battery />
      <Clock />
    </Stack>
  );
};
