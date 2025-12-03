import type { ElementType, HTMLAttributes, PropsWithChildren } from 'react';

export type SurfaceProps = PropsWithChildren<HTMLAttributes<HTMLDivElement>> & {
  as?: ElementType;
};
