import { Battery } from './Battery';
import { Clock } from './Clock';
import { Cpu } from './Cpu';
import { KeepAwake } from './KeepAwake';

import * as styles from './Status.styles';

export const Status = () => {
  return (
    <div className={styles.status}>
      <KeepAwake />
      <Cpu />
      <Battery />
      <Clock />
    </div>
  );
};
