import { cx } from '@linaria/core';

import { useMediaQuery } from '@/hooks';
import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

import * as styles from './Stack.styles';
import type { StackProps } from './Stack.types';

export const Stack = ({ children, className, ...rest }: StackProps) => {
  const isLaptopScreen = useMediaQuery(LAPTOP_MEDIA_QUERY);

  return (
    <div className={cx(styles.stack, isLaptopScreen && styles.stackCompact, className)} {...rest}>
      {children}
    </div>
  );
};
