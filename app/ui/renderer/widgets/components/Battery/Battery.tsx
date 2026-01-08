import { BatteryChargingIcon, BatteryFullIcon, Time02Icon } from '@hugeicons/core-free-icons';

import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';

import { useBatteryWidget } from './Battery.state';
import * as styles from './Battery.styles';

export const Battery = () => {
  const battery = useBatteryWidget();

  if (!battery) {
    return (
      <Surface className={styles.battery}>
        <div className={styles.header}>
          <div className={styles.headerIcon}>
            <Icon icon={BatteryFullIcon} size={20} />
          </div>
          <div className={styles.headerContent}>
            <div className={styles.headerTitle}>Battery</div>
            <div className={styles.headerSubtitle}>No battery detected</div>
          </div>
        </div>
      </Surface>
    );
  }

  const {
    percentage,
    state,
    stateLabel,
    healthFormatted,
    healthColor,
    temperature,
    voltage,
    cycles,
    timeRemaining,
    progressColor,
  } = battery;

  const BatteryIcon = state === 'Charging' ? BatteryChargingIcon : BatteryFullIcon;

  return (
    <Surface className={styles.battery}>
      <div className={styles.header}>
        <div className={styles.headerIcon}>
          <Icon icon={BatteryIcon} size={20} color={progressColor} />
        </div>
        <div className={styles.headerContent}>
          <div className={styles.headerTitle}>{percentage}%</div>
          <div className={styles.headerSubtitle}>{stateLabel}</div>
        </div>
      </div>

      <div className={styles.progressContainer}>
        <div className={styles.progressBar}>
          <div
            className={styles.progressFill}
            style={{
              transform: `scaleX(${percentage / 100})`,
              backgroundColor: progressColor,
            }}
          />
        </div>
        <div className={styles.progressLabel}>
          <span>0%</span>
          <span>100%</span>
        </div>
      </div>

      <div className={styles.stats}>
        <div className={styles.stat}>
          <div className={styles.statLabel}>Health</div>
          <div className={styles.statValue} style={{ color: healthColor }}>
            {healthFormatted}
          </div>
        </div>
        <div className={styles.stat}>
          <div className={styles.statLabel}>Cycles</div>
          <div className={styles.statValue}>{cycles}</div>
        </div>
        <div className={styles.stat}>
          <div className={styles.statLabel}>Temperature</div>
          <div className={styles.statValue}>{temperature}</div>
        </div>
        <div className={styles.stat}>
          <div className={styles.statLabel}>Voltage</div>
          <div className={styles.statValue}>{voltage}</div>
        </div>
      </div>

      {timeRemaining && (
        <div className={styles.timeRemaining}>
          <Icon icon={Time02Icon} size={16} className={styles.timeIcon} />
          <span>{timeRemaining}</span>
        </div>
      )}
    </Surface>
  );
};
