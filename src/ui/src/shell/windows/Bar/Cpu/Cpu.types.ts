import type { AnyIcon } from '../../../components/Icon';

export type CPUInfo = {
  usage: number;
  temperature: number | null;
};

export type CpuState = {
  /** `false` while the reading is unresolved: nothing renders. */
  isResolved: boolean;
  temperature: number | null;
  usage: number;
  color: string;
  icon: AnyIcon;
};
