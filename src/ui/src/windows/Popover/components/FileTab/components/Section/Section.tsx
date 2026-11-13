import { useId, useState, type ReactNode } from 'react';

import { ArrowRight01Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';

import { styles as shared } from '../../styles/form.style';
import { useForm, type Write } from '../../utils/form';
import { pathText, removeOp, type Path } from '../../utils/values';
import { Marker } from '../Marker';
import { Notes } from '../Notes';
import { ResetButton } from '../ResetButton';

import { styles } from './Section.style';

export type SectionProps = {
  path: Path;
  label: string;
  hint: string | null;
  defaultOpen: boolean;
  isSet: boolean;
  inTable: boolean;
  write: Write;
  head?: ReactNode;
  children: ReactNode;
};

/** A collapsible group: nested tables, maps, lists and non-scalar unions. */
export function Section({
  path,
  label,
  hint,
  defaultOpen,
  isSet,
  inTable,
  write,
  head,
  children,
}: SectionProps) {
  const form = useForm();
  const id = useId();
  const [open, setOpen] = useState(defaultOpen);

  return (
    <section
      {...stylex.props(styles.section)}
      data-path={pathText(path)}
      aria-label={label}
      aria-describedby={hint ? id : undefined}
      aria-busy={write.pending}
    >
      <div {...stylex.props(styles.sectionHead)}>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
          {...stylex.props(shared.toggle)}
        >
          <span {...stylex.props(shared.chevron, open && shared.chevronOpen)}>
            <Icon icon={ArrowRight01Icon} size={12} />
          </span>
          {label}
        </button>
        {!isSet && inTable ? <Marker>default</Marker> : null}
        {head}
        {isSet && inTable && !form.readOnly ? (
          <ResetButton
            label={label}
            disabled={write.pending}
            onReset={() => void write.run(removeOp(form.value, form.defaults, path))}
          />
        ) : null}
      </div>
      {hint ? (
        <p id={id} {...stylex.props(styles.sectionHint)}>
          {hint}
        </p>
      ) : null}
      <Notes message={write.message} />
      {open ? <div {...stylex.props(shared.nested)}>{children}</div> : null}
    </section>
  );
}
