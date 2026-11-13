import { useId, type ReactNode } from 'react';

import * as stylex from '@stylexjs/stylex';

import type { JsonValue } from '@/ipc';
import { optionCopy } from '@/windows/Popover/settingsCopy';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { styles as shared } from '../../styles/form.style';
import { useCopy } from '../../utils/copyContext';
import { choice, numberError, variantOptions, type FieldProps } from '../../utils/fields';
import { effectiveAt, isSetAt, useDraft, useForm, useWrite } from '../../utils/form';
import { seed, variantOf } from '../../utils/schema';
import { removeOp, setOp } from '../../utils/values';
import { Notes } from '../Notes';
import { ResetButton } from '../ResetButton';
import { Row } from '../Row';
import { Select } from '../Select';
import { Stepper } from '../Stepper';
import { UnknownValue } from '../UnknownValue';

import { styles } from './ScalarRow.style';

/** One inline control: switch, select, number, text or a constant. */
export function ScalarRow({ node, path, at, label, inTable, actions, plain }: FieldProps) {
  const form = useForm();
  const copy = useCopy(at);
  const id = useId();
  const write = useWrite();
  const value = effectiveAt(form, path, node);
  const isSet = isSetAt(form, path);
  let active = node;
  let optional = false;

  if (active.kind === 'optional') {
    optional = true;
    active = active.inner;
  }

  const union = active.kind === 'union' ? active : null;
  const variant = union ? variantOf(union, value) : -1;

  if (union) {
    active = union.variants[Math.max(variant, 0)].node;
  }

  const unset = optional && value == null;
  const mismatch = union !== null && variant < 0 && !unset;
  const set = (next: JsonValue) => void write.run(setOp(form.value, form.defaults, path, next));
  const save = (raw: string) => {
    if (active.kind === 'number') {
      const error = numberError(active, raw);

      if (error !== null) {
        write.setMessage(error);

        return Promise.resolve({ status: 'error', message: error } as const);
      }

      return write.run(setOp(form.value, form.defaults, path, Number(raw.trim())));
    }

    return write.run(setOp(form.value, form.defaults, path, optional && raw === '' ? null : raw));
  };
  const draft = useDraft(
    typeof value === 'string' || typeof value === 'number' ? String(value) : '',
    save,
    active.kind === 'number' ? (raw) => String(Number(raw.trim())) : undefined,
  );
  const disabled = form.readOnly || write.pending;
  const helpId = `${id}-help`;
  const hint = plain ? null : (copy.hint ?? null);
  let control: ReactNode;

  if (unset) {
    control = (
      <>
        <span {...stylex.props(shared.muted)}>Not set</span>
        <button
          type="button"
          id={id}
          disabled={disabled}
          onClick={() => set(seed(active))}
          {...stylex.props(controls.button)}
        >
          Set
        </button>
      </>
    );
  } else if (mismatch) {
    control = <UnknownValue value={value} />;
  } else if (active.kind === 'boolean') {
    const checked = value === true;

    control = (
      <button
        type="button"
        role="switch"
        id={id}
        aria-checked={checked}
        aria-describedby={hint ? helpId : undefined}
        disabled={disabled}
        onClick={() => set(!checked)}
        {...stylex.props(controls.switch, checked && controls.switchOn)}
      >
        <span {...stylex.props(controls.knob, checked && controls.knobOn)} />
      </button>
    );
  } else if (active.kind === 'enum') {
    control = (
      <Select
        id={id}
        label={label}
        value={typeof value === 'string' ? value : ''}
        disabled={disabled}
        options={active.options.map((option) =>
          choice(option.value, optionCopy(copy.options, option.value)),
        )}
        onSelect={set}
      />
    );
  } else if (active.kind === 'number') {
    const number = active;
    const step = (delta: number) => {
      const current = typeof value === 'number' ? value : 0;
      let next = current + delta;

      next = number.minimum === null ? next : Math.max(number.minimum, next);
      next = number.maximum === null ? next : Math.min(number.maximum, next);
      set(next);
    };

    control = (
      <>
        <input
          id={id}
          type="text"
          inputMode={number.integer ? 'numeric' : 'decimal'}
          aria-describedby={hint ? helpId : undefined}
          aria-invalid={write.message !== null}
          disabled={disabled}
          {...draft.inputProps}
          {...stylex.props(
            shared.input,
            styles.number,
            write.message !== null && shared.inputInvalid,
          )}
        />
        <Stepper
          label={label}
          value={value}
          minimum={number.minimum}
          maximum={number.maximum}
          disabled={disabled}
          onStep={step}
        />
        {copy.unit ? <span {...stylex.props(styles.unit)}>{copy.unit}</span> : null}
      </>
    );
  } else if (active.kind === 'string') {
    control = (
      <input
        id={id}
        type="text"
        spellCheck={false}
        placeholder={optional ? 'not set' : ''}
        aria-describedby={hint ? helpId : undefined}
        aria-invalid={write.message !== null}
        disabled={disabled}
        {...draft.inputProps}
        {...stylex.props(shared.input, write.message !== null && shared.inputInvalid)}
      />
    );
  } else {
    control = <UnknownValue value={value} />;
  }

  // A constant is not a setting; its form is named by the kind menu.
  if (active.kind === 'const') {
    return null;
  }

  const conflict = draft.conflict ? (
    <p {...stylex.props(shared.message, shared.attention)} role="status">
      <span>Changed on disk</span>
      <button
        type="button"
        // Keeps focus in the field, so the blur does not save the draft.
        onPointerDown={(event) => event.preventDefault()}
        onClick={draft.takeDisk}
        {...stylex.props(controls.button)}
      >
        Use disk value
      </button>
    </p>
  ) : null;

  return (
    <Row
      id={id}
      path={path}
      label={label}
      hint={hint}
      helpId={helpId}
      pending={write.pending}
      marker={!isSet && inTable && !unset ? 'default' : null}
      controls={
        <>
          {union && !unset ? (
            <Select
              id={`${id}-kind`}
              label={`${label} kind`}
              value={String(Math.max(variant, 0))}
              disabled={disabled}
              options={variantOptions(union.variants, copy.variants)}
              onSelect={(index) => set(seed(union.variants[Number(index)].node))}
            />
          ) : null}
          {control}
          {isSet && inTable && !form.readOnly ? (
            <ResetButton
              label={label}
              disabled={write.pending}
              onReset={() => void write.run(removeOp(form.value, form.defaults, path))}
            />
          ) : null}
          {actions}
        </>
      }
      notes={<Notes message={write.message} conflict={conflict} />}
    />
  );
}
