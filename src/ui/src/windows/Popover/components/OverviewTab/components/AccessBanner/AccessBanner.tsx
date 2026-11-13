import { Alert02Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import type { HostStatus } from '@/ipc';
import { useOpenAccessibilitySettings, useRequestAccessibility } from '@/queries/access';
import { useOpenAccessibilityGuide } from '@/queries/overview';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { bannerTitle } from '../../utils/modules';

import { styles } from './AccessBanner.style';

/** The Accessibility banner while the gate is blocked, with its three actions. */
export function AccessBanner({ status }: { status: HostStatus }) {
  const guide = useOpenAccessibilityGuide();
  const request = useRequestAccessibility();
  const settings = useOpenAccessibilitySettings();
  const title = bannerTitle(status.access);

  if (title === null) {
    return null;
  }

  return (
    <section {...stylex.props(styles.banner)} aria-labelledby="overview-access" role="region">
      <div {...stylex.props(styles.bannerHead)}>
        <Icon icon={Alert02Icon} size={16} />
        <h3 {...stylex.props(styles.bannerTitle)} id="overview-access">
          {title}
        </h3>
      </div>
      <p {...stylex.props(styles.bannerText)}>
        Stache needs Accessibility to run its modules. Until it is granted every module waits;
        Enable, Pause and Quit keep working.
      </p>
      <div {...stylex.props(styles.bannerActions)}>
        <button
          type="button"
          {...stylex.props(controls.button, controls.primary)}
          disabled={guide.isPending}
          onClick={() => guide.mutate()}
        >
          Show Accessibility Help…
        </button>
        <button
          type="button"
          {...stylex.props(controls.button)}
          disabled={request.isPending}
          onClick={() => request.mutate()}
        >
          Request Accessibility…
        </button>
        <button
          type="button"
          {...stylex.props(controls.button)}
          disabled={settings.isPending}
          onClick={() => settings.mutate()}
        >
          Open Accessibility Settings…
        </button>
      </div>
    </section>
  );
}
