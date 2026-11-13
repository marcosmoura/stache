import type { JsonValue } from '@/ipc';

import { useForm, useWrite } from './form';
import { insertOp, moveOp, removeOp, type Path } from './values';

/** Structural edits of one array, sharing a pending flag and message. */
export function useArrayEdits(path: Path) {
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

export type ArrayEdits = ReturnType<typeof useArrayEdits>;
