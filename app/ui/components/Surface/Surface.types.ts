import type { ComponentPropsWithRef, ComponentType, ElementType, PropsWithChildren } from 'react';

import type { MotionProps } from 'motion/react';

export type SurfaceOwnProps<T extends ElementType = 'div'> = PropsWithChildren<{
  as?: T;
  className?: string;
  animated?: boolean;
}>;

export type SurfaceProps<T extends ElementType = 'div'> = SurfaceOwnProps<T> &
  Omit<ComponentPropsWithRef<T>, keyof SurfaceOwnProps<T>>;

export type MotionableComponent = ComponentType<MotionProps & { className?: string }>;
