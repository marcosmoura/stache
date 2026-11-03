import { useCallback } from 'react';

import { CpuChargeIcon, CpuIcon } from '@hugeicons/core-free-icons';
import { useSuspenseQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';

import type { CPUInfo, CpuState } from './Cpu.types';

function fetchCpu(): Promise<CPUInfo> {
  return invoke<CPUInfo>('get_cpu_info');
}

function isCpuTooHot(temperature: number | null) {
  return temperature && temperature >= 85;
}

function getColor(temperature: number | null) {
  if (isCpuTooHot(temperature)) {
    return colors.red;
  }

  return colors.text;
}

function getIcon(temperature: number | null) {
  if (isCpuTooHot(temperature)) {
    return CpuChargeIcon;
  }

  return CpuIcon;
}

export function useCpu(): CpuState {
  const { data: cpu } = useSuspenseQuery({
    queryKey: ['cpu'],
    queryFn: fetchCpu,
    refetchInterval: 2000,
    refetchOnMount: true,
  });

  const temperature = cpu?.temperature ?? null;
  const usage = cpu?.usage ?? 0;

  const color = getColor(temperature);
  const icon = getIcon(temperature);

  const onCpuClick = useCallback(() => invoke<void>('open_app', { name: 'Activity Monitor' }), []);

  return { temperature, usage, color, icon, onCpuClick };
}
