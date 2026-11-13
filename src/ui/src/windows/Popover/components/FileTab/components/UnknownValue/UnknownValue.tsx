import * as stylex from '@stylexjs/stylex';

import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { useForm } from '../../utils/form';
import { tomlText } from '../../utils/values';

import { styles } from './UnknownValue.style';

/** A value the form cannot edit: its TOML-like text and "Open in editor". */
export function UnknownValue({ value }: { value: unknown }) {
  const form = useForm();

  return (
    <>
      <code {...stylex.props(styles.code)} title={tomlText(value)}>
        {tomlText(value)}
      </code>
      <button type="button" onClick={form.openInEditor} {...stylex.props(controls.button)}>
        Open in editor
      </button>
    </>
  );
}
