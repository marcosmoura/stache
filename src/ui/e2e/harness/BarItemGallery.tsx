import { BatteryFullIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import { bar, color, font } from '@/theme/tokens.stylex';
import { BarItem } from '@/windows/Bar/components/BarItem';

const styles = stylex.create({
  gallery: {
    display: 'flex',
    flexDirection: 'column',
    rowGap: 16,
    alignItems: 'flex-start',
    padding: 16,
  },
  frame: {
    display: 'inline-flex',
    height: 32,
    padding: 4,
    fontFamily: font.family,
    fontSize: bar.fontSize,
    fontWeight: bar.fontWeight,
    fontVariantNumeric: 'tabular-nums',
    lineHeight: bar.lineHeight,
    color: color.text,
    backgroundColor: color.surface,
  },
});

/**
 * `BarItem` in its rest (also hovered by the screenshot spec) and active looks,
 * each in its own framed shot (`/e2e/harness.html?gallery=bar-item`).
 */
export function BarItemGallery() {
  return (
    <div {...stylex.props(styles.gallery)}>
      {(['rest', 'active'] as const).map((look) => (
        <div key={look} data-testid={`shot-${look}`} {...stylex.props(styles.frame)}>
          <BarItem onClick={() => {}} active={look === 'active'}>
            <Icon icon={BatteryFullIcon} />
            85%
          </BarItem>
        </div>
      ))}
    </div>
  );
}
