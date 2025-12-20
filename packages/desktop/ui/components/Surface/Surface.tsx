import type { ElementType } from 'react';

import { cx } from '@linaria/core';

import * as styles from './Surface.styles';
import type { SurfaceProps } from './Surface.types';

export const Surface = <T extends ElementType = 'div'>({
  as,
  children,
  className,
  ...rest
}: SurfaceProps<T>) => {
  const Component = as || 'div';

  return (
    <Component className={cx(styles.surface, className)} {...rest}>
      {children}
    </Component>
  );
};
