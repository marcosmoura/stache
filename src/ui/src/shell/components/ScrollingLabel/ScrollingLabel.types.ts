import type { HTMLAttributes, PropsWithChildren } from 'react';

export type ScrollingLabelProps = PropsWithChildren<HTMLAttributes<HTMLDivElement>> & {
  /**
   * Speed of scrolling in pixels per second.
   * @default 60
   */
  scrollSpeed?: number;
};

export interface ScrollState {
  start: number;
  end: number;
  distance: number;
}
