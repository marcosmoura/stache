import { createContext, use, useRef, useState } from 'react';

import type { ConfigEditOp } from '@/ipc';
import { STALE_MESSAGE, type EditResult } from '@/queries/configDocument';

import type { FieldNode } from './schema';
import { getAt, type Path } from './values';

/** What every field of one file's form shares. */
export type FormContextValue = {
  /** The file's parsed value (only keys present in it), `null` when missing. */
  value: unknown;
  /** The catalog default of the whole file. */
  defaults: unknown;
  /** Invalid or symlinked files are read-only. */
  readOnly: boolean;
  /** Sends one edit through the file's serialized editor. */
  edit: (op: ConfigEditOp) => Promise<EditResult>;
  /** Opens the file in the default app. */
  openInEditor: () => void;
};

export const FormContext = createContext<FormContextValue | null>(null);

export function useForm(): FormContextValue {
  const context = use(FormContext);

  if (context === null) {
    throw new Error('useForm outside a settings form');
  }

  return context;
}

/** Whether an array along `path` is in the file (its elements have no catalog default). */
function throughFileArray(file: unknown, path: Path) {
  return path.some(
    (segment, index) =>
      typeof segment === 'number' && getAt(file, path.slice(0, index)) !== undefined,
  );
}

/** The value a field shows: the file's, else the catalog default, else the schema default. */
export function effectiveAt(form: FormContextValue, path: Path, node: FieldNode): unknown {
  const inFile = getAt(form.value, path);

  if (inFile !== undefined) {
    return inFile;
  }

  const fromDefaults = throughFileArray(form.value, path) ? undefined : getAt(form.defaults, path);

  return fromDefaults ?? node.default;
}

/** Whether the key at `path` is written in the file. */
export const isSetAt = (form: FormContextValue, path: Path) =>
  getAt(form.value, path) !== undefined;

/** The pending flag and inline message of one field's writes. */
export function useWrite() {
  const { edit } = useForm();
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const run = async (op: ConfigEditOp): Promise<EditResult> => {
    setPending(true);
    setMessage(null);
    const result = await edit(op);

    setPending(false);

    if (result.status === 'error') {
      setMessage(result.message);
    } else if (result.status === 'stale') {
      setMessage(STALE_MESSAGE);
    }

    return result;
  };

  return { pending, message, setMessage, run };
}

export type Write = ReturnType<typeof useWrite>;

/**
 * The draft of a text or number field (D8, D10). Typing makes a draft; blur
 * or Enter saves it; Esc drops it (and stops the popover from closing). While
 * the field is focused or has a draft, a new disk value does not replace what
 * is shown: `conflict` is set until the user saves or takes the disk value.
 */
export function useDraft(
  value: string,
  save: (raw: string) => Promise<EditResult>,
  normalise: (raw: string) => string = (raw) => raw,
) {
  const [draft, setDraft] = useState<string | null>(null);
  const [base, setBase] = useState(value);
  const [focused, setFocused] = useState(false);
  const holding = focused || draft !== null;
  const shown = draft ?? (holding ? base : value);
  const conflict = holding && value !== base;
  // The draft being saved: the blur that follows Enter (the field is disabled
  // while the write is pending) must not send it a second time.
  const committing = useRef<string | null>(null);

  const commit = async () => {
    if (draft === null || committing.current === draft) {
      return;
    }

    if (draft === base && !conflict) {
      setDraft(null);

      return;
    }

    committing.current = draft;
    const result = await save(draft).finally(() => {
      committing.current = null;
    });

    if (result.status === 'ok') {
      setDraft(null);
      setBase(normalise(draft));
    }
  };

  return {
    shown,
    draft,
    conflict,
    inputProps: {
      value: shown,
      onFocus: () => {
        if (!focused) {
          setFocused(true);

          if (draft === null) {
            setBase(value);
          }
        }
      },
      onBlur: () => {
        setFocused(false);
        void commit();
      },
      onChange: (event: { currentTarget: { value: string } }) => {
        setDraft(event.currentTarget.value);
      },
      onKeyDown: (event: { key: string; preventDefault: () => void }) => {
        if (event.key === 'Enter') {
          event.preventDefault();
          void commit();
        } else if (event.key === 'Escape' && draft !== null) {
          event.preventDefault();
          setDraft(null);
          setBase(value);
        }
      },
    },
    /** Drops the draft and shows the disk value. */
    takeDisk: () => {
      setDraft(null);
      setBase(value);
    },
  };
}
