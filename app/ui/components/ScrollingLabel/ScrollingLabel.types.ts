import type { HTMLAttributes, PropsWithChildren } from 'react';

export type ScrollingLabelProps = PropsWithChildren<HTMLAttributes<HTMLDivElement>> & {
  /**
   * Speed of scrolling in pixels per second.
   * @default 30
   */
  scrollSpeed?: number;
};
