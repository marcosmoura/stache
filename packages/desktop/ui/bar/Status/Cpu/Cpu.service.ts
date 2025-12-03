import { CpuChargeIcon, CpuIcon } from '@hugeicons/core-free-icons';
import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';

import type { CPUInfo } from './Cpu.types';

export const fetchCpu = (): Promise<CPUInfo> => invoke<CPUInfo>('get_cpu_info');

export const getCPUElements = (temperature: number) => {
  if (temperature >= 85) {
    return {
      color: colors.red,
      icon: CpuChargeIcon,
    };
  }

  return {
    color: colors.text,
    icon: CpuIcon,
  };
};

export const openActivityMonitor = () => invoke('open_app', { name: 'Activity Monitor' });
