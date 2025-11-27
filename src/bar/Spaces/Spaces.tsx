import { CurrentApp } from './CurrentApp';
import { Hyprspace } from './Hyprspace';

import * as styles from './Spaces.styles';

export const Spaces = () => {
  return (
    <div className={styles.spaces}>
      <Hyprspace />
      <CurrentApp />
    </div>
  );
};
