import type { AnyIcon } from '@/components/Icon';

export type CPUInfo = {
  usage: number;
  temperature: number | null;
};

export type CpuState = {
  temperature: number | null;
  usage: number;
  color: string;
  icon: AnyIcon;
  onCpuClick: () => Promise<void>;
};
