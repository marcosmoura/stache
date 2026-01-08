import type { ComponentPropsWithRef, ElementType, PropsWithChildren } from 'react';

export type SurfaceOwnProps<T extends ElementType = 'div'> = PropsWithChildren<{
  as?: T;
  className?: string;
}>;

export type SurfaceProps<T extends ElementType = 'div'> = SurfaceOwnProps<T> &
  Omit<ComponentPropsWithRef<T>, keyof SurfaceOwnProps<T>>;
