import { Battery } from './Battery';
import { Clock } from './Clock';
import { Cpu } from './Cpu';
import { KeepAwake } from './KeepAwake';
import { Weather } from './Weather';

import * as styles from './Status.styles';

export const Status = () => {
  return (
    <div className={styles.status}>
      <Weather />
      <KeepAwake />
      <Cpu />
      <Battery />
      <Clock />
    </div>
  );
};
