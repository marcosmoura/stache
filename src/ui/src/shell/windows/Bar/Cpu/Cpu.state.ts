import { CpuChargeIcon, CpuIcon } from '@hugeicons/core-free-icons';

import { colors } from '../../../../sdk/tokens';
import { useBarData } from '../Bar.data';

import type { CpuState } from './Cpu.types';

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
  const { cpu } = useBarData();

  const temperature = cpu?.temperature ?? null;
  const usage = cpu?.usage ?? 0;

  const color = getColor(temperature);
  const icon = getIcon(temperature);

  return { isResolved: cpu !== undefined, temperature, usage, color, icon };
}
