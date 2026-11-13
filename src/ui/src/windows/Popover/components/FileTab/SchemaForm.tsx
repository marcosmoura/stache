import { useId, useState, type ReactNode } from 'react';

import {
  Add01Icon,
  ArrowDown01Icon,
  ArrowRight01Icon,
  ArrowUp01Icon,
  Delete02Icon,
  DragDropVerticalIcon,
  Undo02Icon,
} from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import type { JsonValue, PathSegment } from '@/ipc';

import { styles as overview } from '../OverviewTab/OverviewTab.style';

import { itemTitle } from './FileTab.model';
import { styles } from './FileTab.style';
import { effectiveAt, isSetAt, useDraft, useForm, useWrite, type Write } from './form';
import { isScalar, seed, variantOf, type FieldNode, type Variant } from './schema';
import { Select } from './Select';
import { summary } from './summary';
import {
  humanise,
  insertOp,
  moveOp,
  pathText,
  removeOp,
  renameOp,
  setOp,
  tomlText,
  type Path,
} from './values';

/*
 * The generic settings form (D6–D8): one component per control kind, all
 * driven by the field tree. A field shows the file's value, else the default
 * (marked "default"); a set key inside a table has "Reset to default"
 * (`remove`). Switches, selects, steppers and structural actions write at
 * once; text and numbers on blur or Enter.
 */

type FieldProps = {
  node: FieldNode;
  path: Path;
  label: string;
  /** The field is a key of a table (it can be default and be reset). */
  inTable: boolean;
  depth: number;
  /** Render without the section header (array cards, map values). */
  bare?: boolean;
  /** Extra trailing controls (a list item's move and remove). */
  actions?: ReactNode;
};

/** The whole form of one file. */
export function SchemaForm({ tree }: { tree: FieldNode }) {
  return (
    <div {...stylex.props(styles.form)} data-testid="settings-form">
      {tree.kind === 'object' ? (
        <ObjectBody node={tree} path={[]} depth={0} />
      ) : (
        <Field node={tree} path={[]} label="Value" inTable={false} depth={0} />
      )}
    </div>
  );
}

function Field(props: FieldProps) {
  const { node } = props;

  if (isScalar(node)) {
    return <ScalarRow {...props} />;
  }

  switch (node.kind) {
    case 'optional':
      return <OptionalBlock {...props} node={node} />;
    case 'union':
      return <UnionBlock {...props} node={node} />;
    case 'object':
    case 'map':
    case 'array':
      return <TableBlock {...props} />;
    default:
      return <UnknownRow {...props} />;
  }
}

/** Groups consecutive scalar keys on one card; nested values follow as sections. */
function ObjectBody({
  node,
  path,
  depth,
}: {
  node: FieldNode & { kind: 'object' };
  path: Path;
  depth: number;
}) {
  const groups: Array<{ scalar: boolean; keys: typeof node.properties }> = [];

  for (const property of node.properties) {
    const scalar = isScalar(property.node);
    const last = groups.at(-1);

    if (last && last.scalar && scalar) {
      last.keys.push(property);
    } else {
      groups.push({ scalar, keys: [property] });
    }
  }

  return (
    <>
      {groups.map((group) => {
        const fields = group.keys.map((property) => (
          <Field
            key={property.key}
            node={property.node}
            path={[...path, property.key]}
            label={humanise(property.key)}
            inTable
            depth={depth + 1}
          />
        ));

        return group.scalar ? (
          <div key={group.keys[0].key} {...stylex.props(styles.card)}>
            {fields}
          </div>
        ) : (
          fields
        );
      })}
    </>
  );
}

function Marker({ children }: { children: ReactNode }) {
  return <span {...stylex.props(styles.marker)}>{children}</span>;
}

function Notes({ message, conflict }: { message: string | null; conflict?: ReactNode }) {
  if (message === null && !conflict) {
    return null;
  }

  return (
    <>
      {message !== null ? (
        <p {...stylex.props(styles.message, styles.danger)} role="alert">
          {message}
        </p>
      ) : null}
      {conflict}
    </>
  );
}

function ResetButton({
  label,
  onReset,
  disabled,
}: {
  label: string;
  onReset: () => void;
  disabled: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={`Reset ${label} to default`}
      title="Reset to default"
      disabled={disabled}
      onClick={onReset}
      {...stylex.props(overview.round)}
    >
      <Icon icon={Undo02Icon} size={14} />
    </button>
  );
}

function RoundButton({
  label,
  icon,
  disabled,
  onClick,
}: {
  label: string;
  icon: typeof Add01Icon;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      {...stylex.props(overview.round)}
    >
      <Icon icon={icon} size={14} />
    </button>
  );
}

function numberError(node: FieldNode & { kind: 'number' }, raw: string): string | null {
  const parsed = Number(raw.trim());

  if (raw.trim() === '' || Number.isNaN(parsed)) {
    return 'Enter a number.';
  }

  if (node.integer && !Number.isInteger(parsed)) {
    return 'Enter a whole number.';
  }

  if (node.minimum !== null && parsed < node.minimum) {
    return `Must be at least ${node.minimum}.`;
  }

  if (node.maximum !== null && parsed > node.maximum) {
    return `Must be at most ${node.maximum}.`;
  }

  return null;
}

const variantOptions = (variants: readonly Variant[]) =>
  variants.map((variant, index) => ({
    value: String(index),
    title: variant.label,
    description: variant.description,
  }));

/** One inline control: switch, select, number, text or a constant. */
function ScalarRow({ node, path, label, inTable, actions }: FieldProps) {
  const form = useForm();
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
  const description = node.description ?? active.description;
  let control: ReactNode;

  if (unset) {
    control = (
      <>
        <span {...stylex.props(styles.muted)}>Not set</span>
        <button
          type="button"
          id={id}
          disabled={disabled}
          onClick={() => set(seed(active))}
          {...stylex.props(overview.button)}
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
        aria-describedby={description ? helpId : undefined}
        disabled={disabled}
        onClick={() => set(!checked)}
        {...stylex.props(overview.switch, checked && overview.switchOn)}
      >
        <span {...stylex.props(overview.knob, checked && overview.knobOn)} />
      </button>
    );
  } else if (active.kind === 'enum') {
    control = (
      <Select
        id={id}
        label={label}
        value={typeof value === 'string' ? value : ''}
        disabled={disabled}
        options={active.options.map((option) => ({
          value: option.value,
          title: option.value,
          description: option.description,
        }))}
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
          aria-describedby={description ? helpId : undefined}
          aria-invalid={write.message !== null}
          disabled={disabled}
          {...draft.inputProps}
          {...stylex.props(
            styles.input,
            styles.number,
            write.message !== null && styles.inputInvalid,
          )}
        />
        <span {...stylex.props(styles.stepper)}>
          <button
            type="button"
            aria-label={`Increase ${label}`}
            title={`Increase ${label}`}
            disabled={disabled || (number.maximum !== null && value === number.maximum)}
            onClick={() => step(1)}
            {...stylex.props(styles.step)}
          >
            <Icon icon={ArrowUp01Icon} size={9} strokeWidth={2.5} />
          </button>
          <button
            type="button"
            aria-label={`Decrease ${label}`}
            title={`Decrease ${label}`}
            disabled={disabled || (number.minimum !== null && value === number.minimum)}
            onClick={() => step(-1)}
            {...stylex.props(styles.step)}
          >
            <Icon icon={ArrowDown01Icon} size={9} strokeWidth={2.5} />
          </button>
        </span>
      </>
    );
  } else if (active.kind === 'string') {
    control = (
      <input
        id={id}
        type="text"
        spellCheck={false}
        placeholder={optional ? 'not set' : ''}
        aria-describedby={description ? helpId : undefined}
        aria-invalid={write.message !== null}
        disabled={disabled}
        {...draft.inputProps}
        {...stylex.props(styles.input, write.message !== null && styles.inputInvalid)}
      />
    );
  } else if (active.kind === 'const') {
    control = <code {...stylex.props(styles.code)}>{tomlText(active.value)}</code>;
  } else {
    control = <UnknownValue value={value} />;
  }

  const conflict = draft.conflict ? (
    <p {...stylex.props(styles.message, styles.attention)} role="status">
      <span>Changed on disk</span>
      <button
        type="button"
        // Keeps focus in the field, so the blur does not save the draft.
        onPointerDown={(event) => event.preventDefault()}
        onClick={draft.takeDisk}
        {...stylex.props(overview.button)}
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
      description={description}
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
              options={variantOptions(union.variants)}
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

function Row({
  id,
  path,
  label,
  description,
  helpId,
  pending,
  marker,
  controls,
  notes,
}: {
  id: string;
  path: Path;
  label: string;
  description: string | null;
  helpId: string;
  pending: boolean;
  marker: string | null;
  controls: ReactNode;
  notes?: ReactNode;
}) {
  return (
    <div
      {...stylex.props(styles.row, pending && styles.rowPending)}
      data-path={pathText(path)}
      aria-busy={pending}
    >
      <div {...stylex.props(styles.text)}>
        <span {...stylex.props(styles.labelLine)}>
          <label htmlFor={id} {...stylex.props(styles.label)}>
            {label}
          </label>
          {marker ? <Marker>{marker}</Marker> : null}
        </span>
        {description ? (
          <>
            <span {...stylex.props(styles.help)} title={description} aria-hidden>
              {summary(description)}
            </span>
            <span id={helpId} {...stylex.props(styles.visuallyHidden)}>
              {description}
            </span>
          </>
        ) : null}
      </div>
      <div {...stylex.props(styles.controls)}>{controls}</div>
      {notes}
    </div>
  );
}

function UnknownValue({ value }: { value: unknown }) {
  const form = useForm();

  return (
    <>
      <code {...stylex.props(styles.code)} title={tomlText(value)}>
        {tomlText(value)}
      </code>
      <button type="button" onClick={form.openInEditor} {...stylex.props(overview.button)}>
        Open in editor
      </button>
    </>
  );
}

/** A construct the walk did not recognise: its TOML text and "Open in editor". */
function UnknownRow({ node, path, label }: FieldProps) {
  const form = useForm();
  const id = useId();

  return (
    <Row
      id={id}
      path={path}
      label={label}
      description={node.description}
      helpId={`${id}-help`}
      pending={false}
      marker={null}
      controls={<UnknownValue value={effectiveAt(form, path, node)} />}
    />
  );
}

/** A collapsible group: nested tables, maps, lists and non-scalar unions. */
function Section({
  path,
  label,
  description,
  defaultOpen,
  isSet,
  inTable,
  write,
  head,
  children,
}: {
  path: Path;
  label: string;
  description: string | null;
  defaultOpen: boolean;
  isSet: boolean;
  inTable: boolean;
  write: Write;
  head?: ReactNode;
  children: ReactNode;
}) {
  const form = useForm();
  const [open, setOpen] = useState(defaultOpen);

  return (
    <section
      {...stylex.props(styles.section)}
      data-path={pathText(path)}
      aria-label={label}
      aria-busy={write.pending}
    >
      <div {...stylex.props(styles.sectionHead)}>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
          title={description ?? undefined}
          {...stylex.props(styles.toggle)}
        >
          <span {...stylex.props(styles.chevron, open && styles.chevronOpen)}>
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
      <Notes message={write.message} />
      {open ? <div {...stylex.props(styles.nested)}>{children}</div> : null}
    </section>
  );
}

/** The body of a table, map or list (without a header). */
function Body({ node, path, label, depth }: FieldProps) {
  switch (node.kind) {
    case 'object':
      return <ObjectBody node={node} path={path} depth={depth} />;
    case 'map':
      return <MapBody node={node} path={path} depth={depth} />;
    case 'array':
      return isScalar(node.items) ? (
        <ListBody node={node} path={path} label={label} depth={depth} />
      ) : (
        <CardsBody node={node} path={path} depth={depth} />
      );
    default:
      return <Field node={node} path={path} label={label} inTable={false} depth={depth} bare />;
  }
}

/** A nested table, a map or a list: a section around its body. */
function TableBlock(props: FieldProps) {
  const { node, path, label, inTable, depth, bare } = props;
  const form = useForm();
  const write = useWrite();

  if (bare) {
    return <Body {...props} />;
  }

  const value = effectiveAt(form, path, node);
  const count = Array.isArray(value) ? value.length : null;

  return (
    <Section
      path={path}
      label={label}
      description={node.description}
      // Long lists start collapsed; deeper tables too.
      defaultOpen={depth <= 1 && (count === null || count <= 8)}
      isSet={isSetAt(form, path)}
      inTable={inTable}
      write={write}
      head={count !== null ? <Marker>{count}</Marker> : null}
    >
      <Body {...props} />
    </Section>
  );
}

/** A non-scalar union: a kind select in the header, the active variant's body below. */
function UnionBlock({
  node,
  path,
  label,
  inTable,
  depth,
  bare,
}: FieldProps & { node: FieldNode & { kind: 'union' } }) {
  const form = useForm();
  const id = useId();
  const write = useWrite();
  const value = effectiveAt(form, path, node);
  const variant = variantOf(node, value);
  const kind = (
    <Select
      id={id}
      label={`${label} kind`}
      value={String(Math.max(variant, 0))}
      disabled={form.readOnly || write.pending}
      options={variantOptions(node.variants)}
      onSelect={(index) =>
        void write.run(
          setOp(form.value, form.defaults, path, seed(node.variants[Number(index)].node)),
        )
      }
    />
  );
  const active = variant < 0 ? null : node.variants[variant].node;
  const body =
    active === null ? (
      <UnknownRow node={node} path={path} label={label} inTable={false} depth={depth} />
    ) : isScalar(active) ? (
      <ScalarRow node={active} path={path} label="Value" inTable={false} depth={depth} />
    ) : (
      <Body node={active} path={path} label={label} inTable={false} depth={depth} />
    );

  if (bare) {
    return (
      <>
        <div {...stylex.props(styles.row)} data-path={pathText(path)}>
          <span {...stylex.props(styles.label)}>Kind</span>
          <div {...stylex.props(styles.controls)}>{kind}</div>
          <Notes message={write.message} />
        </div>
        {body}
      </>
    );
  }

  return (
    <Section
      path={path}
      label={label}
      description={node.description}
      defaultOpen={depth <= 2}
      isSet={isSetAt(form, path)}
      inTable={inTable}
      write={write}
      head={kind}
    >
      {body}
    </Section>
  );
}

/** An optional table or union: "Not set" with Set, else the inner field. */
function OptionalBlock(props: FieldProps & { node: FieldNode & { kind: 'optional' } }) {
  const { node, path, label, inTable } = props;
  const form = useForm();
  const id = useId();
  const write = useWrite();
  const value = effectiveAt(form, path, node);

  if (value != null) {
    return (
      <Field
        {...props}
        node={{ ...node.inner, description: node.description ?? node.inner.description }}
        inTable={inTable}
      />
    );
  }

  return (
    <Row
      id={id}
      path={path}
      label={label}
      description={node.description}
      helpId={`${id}-help`}
      pending={write.pending}
      marker={null}
      controls={
        <>
          <span {...stylex.props(styles.muted)}>Not set</span>
          <button
            type="button"
            id={id}
            disabled={form.readOnly || write.pending}
            onClick={() => void write.run(setOp(form.value, form.defaults, path, seed(node.inner)))}
            {...stylex.props(overview.button)}
          >
            Set
          </button>
        </>
      }
      notes={<Notes message={write.message} />}
    />
  );
}

/** Key/value rows: rename the key, edit the value, remove, add. */
function MapBody({
  node,
  path,
  depth,
}: {
  node: FieldNode & { kind: 'map' };
  path: Path;
  depth: number;
}) {
  const form = useForm();
  const write = useWrite();
  const value = effectiveAt(form, path, node);
  const keys = value && typeof value === 'object' ? Object.keys(value) : [];
  const add = () => {
    let key = 'new_key';

    for (let index = 2; keys.includes(key); index += 1) {
      key = `new_key_${index}`;
    }

    void write.run(setOp(form.value, form.defaults, [...path, key], seed(node.values)));
  };

  return (
    <>
      <ul {...stylex.props(styles.cards)}>
        {keys.map((key) => (
          <MapEntry key={key} node={node} path={path} entryKey={key} keys={keys} depth={depth} />
        ))}
      </ul>
      <Notes message={write.message} />
      <div {...stylex.props(styles.actions)}>
        <button
          type="button"
          disabled={form.readOnly || write.pending}
          onClick={add}
          {...stylex.props(overview.button)}
        >
          Add entry
        </button>
      </div>
    </>
  );
}

function MapEntry({
  node,
  path,
  entryKey,
  keys,
  depth,
}: {
  node: FieldNode & { kind: 'map' };
  path: Path;
  entryKey: string;
  keys: readonly string[];
  depth: number;
}) {
  const form = useForm();
  const write = useWrite();
  const draft = useDraft(entryKey, (raw) => {
    const next = raw.trim();
    const error =
      next === ''
        ? 'Enter a key.'
        : keys.includes(next) && next !== entryKey
          ? `"${next}" already exists.`
          : null;

    if (error !== null) {
      write.setMessage(error);

      return Promise.resolve({ status: 'error', message: error } as const);
    }

    return write.run(renameOp(form.value, form.defaults, path, entryKey, next));
  });

  return (
    <li
      {...stylex.props(styles.item)}
      role="group"
      aria-label={entryKey}
      data-path={pathText([...path, entryKey])}
    >
      <div {...stylex.props(styles.itemHead)}>
        <input
          type="text"
          aria-label="Key"
          spellCheck={false}
          disabled={form.readOnly || write.pending}
          {...draft.inputProps}
          {...stylex.props(styles.input, styles.wide)}
        />
        <RoundButton
          label={`Remove ${entryKey}`}
          icon={Delete02Icon}
          disabled={form.readOnly || write.pending}
          onClick={() => void write.run(removeOp(form.value, form.defaults, [...path, entryKey]))}
        />
      </div>
      <Notes message={write.message} />
      <Field
        node={node.values}
        path={[...path, entryKey]}
        label="Value"
        inTable={false}
        depth={depth + 1}
        bare
      />
    </li>
  );
}

/** Structural edits of one array, sharing a pending flag and message. */
function useArrayEdits(path: Path) {
  const form = useForm();
  const write = useWrite();

  return {
    write,
    disabled: form.readOnly || write.pending,
    insert: (index: number, value: JsonValue) =>
      void write.run(insertOp(form.value, form.defaults, path, index, value)),
    move: (from: number, to: number) =>
      void write.run(moveOp(form.value, form.defaults, path, from, to)),
    remove: (index: number) =>
      void write.run(removeOp(form.value, form.defaults, [...path, index])),
  };
}

function ItemActions({
  title,
  index,
  count,
  edits,
}: {
  title: string;
  index: number;
  count: number;
  edits: ReturnType<typeof useArrayEdits>;
}) {
  return (
    <>
      <RoundButton
        label={`Move ${title} up`}
        icon={ArrowUp01Icon}
        disabled={edits.disabled || index === 0}
        onClick={() => edits.move(index, index - 1)}
      />
      <RoundButton
        label={`Move ${title} down`}
        icon={ArrowDown01Icon}
        disabled={edits.disabled || index === count - 1}
        onClick={() => edits.move(index, index + 1)}
      />
      <RoundButton
        label={`Remove ${title}`}
        icon={Delete02Icon}
        disabled={edits.disabled}
        onClick={() => edits.remove(index)}
      />
    </>
  );
}

/** A list of scalars: one row per item with move and remove, then Add. */
function ListBody({
  node,
  path,
  label,
  depth,
}: {
  node: FieldNode & { kind: 'array' };
  path: Path;
  label: string;
  depth: number;
}) {
  const form = useForm();
  const edits = useArrayEdits(path);
  const value = effectiveAt(form, path, node);
  const items = Array.isArray(value) ? value : [];

  return (
    <>
      <div {...stylex.props(styles.card)}>
        {items.map((_, index) => (
          <ScalarRow
            key={index}
            node={node.items}
            path={[...path, index]}
            label={`${label} ${index + 1}`}
            inTable={false}
            depth={depth + 1}
            actions={
              <ItemActions
                title={`${label} ${index + 1}`}
                index={index}
                count={items.length}
                edits={edits}
              />
            }
          />
        ))}
        {items.length === 0 ? <p {...stylex.props(styles.message, styles.muted)}>Empty</p> : null}
      </div>
      <Notes message={edits.write.message} />
      <div {...stylex.props(styles.actions)}>
        <button
          type="button"
          disabled={edits.disabled}
          onClick={() => edits.insert(items.length, seed(node.items))}
          {...stylex.props(overview.button)}
        >
          Add {label.toLowerCase()} item
        </button>
      </div>
    </>
  );
}

/** An array of tables: collapsible cards with add, remove, drag and keyboard reorder. */
function CardsBody({
  node,
  path,
  depth,
}: {
  node: FieldNode & { kind: 'array' };
  path: Path;
  depth: number;
}) {
  const form = useForm();
  const edits = useArrayEdits(path);
  const value = effectiveAt(form, path, node);
  const items = Array.isArray(value) ? value : [];

  return (
    <>
      <ul {...stylex.props(styles.cards)}>
        {items.map((item, index) => (
          <Card
            key={index}
            node={node.items}
            path={[...path, index]}
            index={index}
            count={items.length}
            title={itemTitle(item, index)}
            depth={depth}
            edits={edits}
          />
        ))}
      </ul>
      <Notes message={edits.write.message} />
      <div {...stylex.props(styles.actions)}>
        <button
          type="button"
          disabled={edits.disabled}
          onClick={() => edits.insert(items.length, seed(node.items))}
          {...stylex.props(overview.button)}
        >
          Add item
        </button>
      </div>
    </>
  );
}

function Card({
  node,
  path,
  index,
  count,
  title,
  depth,
  edits,
}: {
  node: FieldNode;
  path: readonly PathSegment[];
  index: number;
  count: number;
  title: string;
  depth: number;
  edits: ReturnType<typeof useArrayEdits>;
}) {
  const [open, setOpen] = useState(false);
  const [dropping, setDropping] = useState(false);

  return (
    <li
      {...stylex.props(styles.item, dropping && styles.dropTarget)}
      role="group"
      aria-label={title}
      data-path={pathText(path)}
      onDragOver={(event) => {
        if (!edits.disabled) {
          event.preventDefault();
          setDropping(true);
        }
      }}
      onDragLeave={() => setDropping(false)}
      onDrop={(event) => {
        event.preventDefault();
        setDropping(false);
        const from = Number(event.dataTransfer.getData('text/plain'));

        if (Number.isInteger(from) && from !== index) {
          edits.move(from, index);
        }
      }}
    >
      <div {...stylex.props(styles.itemHead)}>
        <span
          {...stylex.props(styles.handle)}
          draggable={!edits.disabled}
          aria-hidden
          onDragStart={(event) => {
            event.dataTransfer.setData('text/plain', String(index));
            event.dataTransfer.effectAllowed = 'move';
          }}
        >
          <Icon icon={DragDropVerticalIcon} size={14} />
        </span>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
          {...stylex.props(styles.toggle)}
        >
          <span {...stylex.props(styles.chevron, open && styles.chevronOpen)}>
            <Icon icon={ArrowRight01Icon} size={12} />
          </span>
          {title}
        </button>
        <ItemActions title={title} index={index} count={count} edits={edits} />
      </div>
      {open ? (
        <div {...stylex.props(styles.nested)}>
          <Field node={node} path={path} label={title} inTable={false} depth={depth + 1} bare />
        </div>
      ) : null}
    </li>
  );
}
