import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { useCompact } from '@/hooks';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { batteryIcon, batteryLabel, batteryTone, selectBattery } from './Battery.state';

/**
 * Battery charge and state. Hidden without a reading (no battery, or not
 * sampled yet). Click: `bar_action("battery")`.
 */
export function Battery(_props: WidgetProps<'battery'>) {
  const { data: reading } = useQuery({ ...barSnapshotOptions(), select: selectBattery });
  const compact = useCompact();
  const run = useBarAction();

  if (!reading) {
    return null;
  }

  return (
    <BarItem onClick={() => run('battery')}>
      <Icon
        icon={batteryIcon(reading)}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon.root, widgetIcon[batteryTone(reading)]]}
      />
      <span>{batteryLabel(reading, compact)}</span>
    </BarItem>
  );
}

export default Battery;
