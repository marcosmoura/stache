import { cx } from '@linaria/core';

import * as styles from './Surface.styles';
import type { SurfaceProps } from './Surface.types';

export const Surface = ({ as, children, className, ...rest }: SurfaceProps) => {
  const Component = as || 'div';

  return (
    <Component className={cx(styles.surface, className)} {...rest}>
      {children}
    </Component>
  );
};
