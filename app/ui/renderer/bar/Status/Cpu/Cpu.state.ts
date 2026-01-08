import { useCallback, useMemo } from 'react';

import { CpuChargeIcon, CpuIcon } from '@hugeicons/core-free-icons';
import { useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';

import type { CPUInfo } from './Cpu.types';

const fetchCpu = (): Promise<CPUInfo> => invoke<CPUInfo>('get_cpu_info');

export const useCpu = () => {
  const { data: cpu } = useSuspenseQuery({
    queryKey: ['cpu'],
    queryFn: fetchCpu,
    refetchInterval: 2000, // 2 seconds
    refetchOnMount: true,
  });

  const temperature = cpu?.temperature ?? null;
  const usage = cpu?.usage ?? 0;

  const { color, icon } = useMemo(() => {
    if (temperature && temperature >= 85) {
      return {
        color: colors.red,
        icon: CpuChargeIcon,
      };
    }

    return {
      color: colors.text,
      icon: CpuIcon,
    };
  }, [temperature]);

  const onCpuClick = useCallback(() => invoke('open_app', { name: 'Activity Monitor' }), []);

  return { temperature, usage, color, icon, onCpuClick };
};
