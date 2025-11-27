import { Clock } from './Clock';

import * as styles from './Status.styles';

export const Status = () => {
  return (
    <div className={styles.status}>
      <Clock />
    </div>
  );
};
