import { type ElementType } from 'react';

import { cx } from '@linaria/core';
import { type DOMMotionComponents, motion } from 'motion/react';

import { animate, getMotionComponent, initial, transition } from './Surface.motion';
import * as styles from './Surface.styles';
import type { MotionableComponent, SurfaceProps } from './Surface.types';

const renderCustomMotionSurface = (
  Component: MotionableComponent,
  props: Record<string, unknown>,
) => {
  const MotionComponent = getMotionComponent(Component);

  return <MotionComponent initial={initial} animate={animate} transition={transition} {...props} />;
};

export const Surface = <T extends ElementType = 'div'>({
  as,
  className,
  animated = true,
  ...rest
}: SurfaceProps<T>) => {
  const Component = as ?? 'div';
  const combinedClassName = cx(styles.surface, className);
  const props = {
    ...rest,
    className: combinedClassName,
  };

  if (!animated) {
    return <Component {...props} />;
  }

  if (typeof Component === 'string') {
    const MotionElement = motion[Component as keyof DOMMotionComponents];

    return <MotionElement initial={initial} animate={animate} transition={transition} {...props} />;
  }

  return renderCustomMotionSurface(Component as MotionableComponent, props);
};
