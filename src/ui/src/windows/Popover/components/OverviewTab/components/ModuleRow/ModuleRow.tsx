import { File01Icon, PauseIcon, PlayIcon, RefreshIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import type { ModuleStatusView, SessionAction } from '@/ipc';
import { styles as controls } from '@/windows/Popover/styles/controls.style';
import { FILE_ICONS } from '@/windows/Popover/utils/tabs';

import { styles as row } from '../../styles/row.style';
import {
  moduleActions,
  moduleFile,
  moduleName,
  STATE_LABELS,
  STATE_TONES,
} from '../../utils/modules';

import { styles } from './ModuleRow.style';

export type ModuleRowProps = {
  module: ModuleStatusView;
  pending: 'enable' | 'retry' | 'session' | null;
  error: string | null;
  hasTab: boolean;
  onSelect: () => void;
  onToggle: () => void;
  onSession: (action: SessionAction) => void;
  onRetry: () => void;
};

/** One module: icon, name, state, detail, diagnostic, inline error and its controls. */
export function ModuleRow({
  module,
  pending,
  error,
  hasTab,
  onSelect,
  onToggle,
  onSession,
  onRetry,
}: ModuleRowProps) {
  const name = moduleName(module.id);
  const actions = moduleActions(module);
  const busy = pending !== null;
  const tone = STATE_TONES[module.state];

  return (
    <li
      {...stylex.props(row.row)}
      data-module={module.id}
      data-state={module.state}
      data-pending={pending ?? undefined}
      aria-busy={busy}
    >
      <span {...stylex.props(row.tile)}>
        <Icon icon={FILE_ICONS[moduleFile(module.id)] ?? File01Icon} size={16} />
      </span>
      <div {...stylex.props(row.text)}>
        <div {...stylex.props(styles.titleLine)}>
          {hasTab ? (
            <button
              type="button"
              {...stylex.props(row.name, styles.nameButton)}
              onClick={onSelect}
              title={`Open ${moduleFile(module.id)}`}
            >
              {name}
            </button>
          ) : (
            <span {...stylex.props(row.name)}>{name}</span>
          )}
          <span {...stylex.props(styles.state)} data-testid="state" data-tone={tone}>
            <span {...stylex.props(styles.dot, styles[tone])} aria-hidden="true" />
            {STATE_LABELS[module.state]}
          </span>
        </div>
        {module.detail ? (
          <span {...stylex.props(row.secondary)} title={module.detail}>
            {module.detail}
          </span>
        ) : null}
        {module.diagnostic ? (
          <span
            {...stylex.props(row.secondary, row.attention)}
            title={`configuration: ${module.diagnostic}`}
          >
            configuration: {module.diagnostic}
          </span>
        ) : null}
        {error ? (
          <span {...stylex.props(row.secondary, row.danger)} role="alert" title={error}>
            {error}
          </span>
        ) : null}
      </div>
      <div {...stylex.props(styles.controls)}>
        {actions.retry ? (
          <button
            type="button"
            {...stylex.props(controls.round)}
            aria-label={`Retry ${name}`}
            title={`Retry ${name}`}
            disabled={busy}
            onClick={onRetry}
          >
            <Icon icon={RefreshIcon} size={14} />
          </button>
        ) : null}
        {actions.session ? (
          <button
            type="button"
            {...stylex.props(controls.round)}
            aria-label={`${actions.session === 'pause' ? 'Pause' : 'Resume'} ${name}`}
            title={`${actions.session === 'pause' ? 'Pause' : 'Resume'} ${name}`}
            disabled={busy}
            onClick={() => onSession(actions.session ?? 'pause')}
          >
            <Icon icon={actions.session === 'pause' ? PauseIcon : PlayIcon} size={14} />
          </button>
        ) : null}
        <button
          type="button"
          role="switch"
          aria-checked={module.enabled}
          aria-label={`Enable ${name}`}
          title={`${module.enabled ? 'Disable' : 'Enable'} ${name}`}
          disabled={busy}
          {...stylex.props(controls.switch, module.enabled && controls.switchOn)}
          onClick={onToggle}
        >
          <span {...stylex.props(controls.knob, module.enabled && controls.knobOn)} />
        </button>
      </div>
    </li>
  );
}
