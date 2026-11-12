import { CpuChargeIcon, CpuIcon } from '@hugeicons/core-free-icons';
import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { cpuTemperature, isCpuAlert, selectCpu } from './Cpu.state';

/**
 * CPU usage and temperature. At or above `temperature_alert` the icon is the
 * "charge" variant in the danger color. Hidden until the first reading.
 * Click: `bar_action("cpu")`.
 */
export function Cpu({ options }: WidgetProps<'cpu'>) {
  const { data: reading } = useQuery({ ...barSnapshotOptions(), select: selectCpu });
  const run = useBarAction();

  if (!reading) {
    return null;
  }

  const alert = isCpuAlert(reading, options.temperature_alert);
  const temperature = cpuTemperature(reading);

  return (
    <BarItem onClick={() => run('cpu')}>
      <Icon
        icon={alert ? CpuChargeIcon : CpuIcon}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon.root, alert ? widgetIcon.danger : widgetIcon.text]}
      />
      <span>{reading.usage}%</span>
      {temperature !== null && <span>{temperature}°C</span>}
    </BarItem>
  );
}

export default Cpu;
