import { useQuery } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { barSnapshotOptions } from '@/queries/bar';
import { useBarAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';

import { WIDGET_ICON_STROKE, widgetIcon } from '../../styles/widgetIcon.style';
import type { WidgetProps } from '../types';

import { selectWeatherKey, weatherOptions, weatherStateOptions } from './Weather.query';
import { weatherLook } from './Weather.state';

/**
 * Current weather: condition icon and feels-like temperature, `Loading...`
 * while configured without conditions, hidden while unconfigured. Runs only
 * while the Bar is available. Click: `bar_action("weather")`.
 */
export function Weather(_props: WidgetProps<'weather'>) {
  const { data: inputs = null } = useQuery({ ...barSnapshotOptions(), select: selectWeatherKey });
  const { data: result } = useQuery(weatherOptions(inputs));
  const { data: state } = useQuery(weatherStateOptions(inputs?.configRevision ?? null));
  const run = useBarAction();
  const look = inputs ? weatherLook(result, state) : null;

  if (!look) {
    return null;
  }

  return (
    <BarItem onClick={() => run('weather')}>
      <Icon
        icon={look.icon}
        strokeWidth={WIDGET_ICON_STROKE}
        style={[widgetIcon.root, widgetIcon.text]}
      />
      <span>{look.label}</span>
    </BarItem>
  );
}

export default Weather;
