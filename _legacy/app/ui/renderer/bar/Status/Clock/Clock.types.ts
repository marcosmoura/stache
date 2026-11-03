import type { RefObject } from 'react';

export type ClockState = {
  clock: string;
  ref: RefObject<HTMLButtonElement | null>;
  onClick: () => void;
};
