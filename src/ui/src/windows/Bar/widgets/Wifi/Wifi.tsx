import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { ScrollingLabel } from '@/components/ScrollingLabel';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { selectWifi, wifiLook } from './Wifi.state';
import { styles } from './Wifi.style';

/**
 * Wi-Fi state: signal icon and network name while connected, a labelled icon
 * when disconnected or off. Hidden while the status is unknown. Click:
 * `bar_action("wifi")`.
 */
export function Wifi(_props: WidgetProps<'wifi'>) {
  const { data: reading = null } = useQuery({ ...barSnapshotOptions(), select: selectWifi });
  const run = useBarAction();
  const look = wifiLook(reading);

  if (!look) {
    return null;
  }

  return (
    <BarItem
      onClick={() => run('wifi')}
      label={look.label === null ? 'Wi-Fi connected' : undefined}
    >
      <Icon
        icon={look.icon}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon.root, widgetIcon[look.tone]]}
      />
      {look.label !== null && <ScrollingLabel style={styles.label}>{look.label}</ScrollingLabel>}
    </BarItem>
  );
}

export default Wifi;
