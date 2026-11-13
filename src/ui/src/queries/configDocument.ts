import {
  mutationOptions,
  queryOptions,
  useMutation,
  type QueryClient,
} from '@tanstack/react-query';

import {
  commands,
  type ConfigDocumentOutcome_Serialize,
  type ConfigEditOp,
  type ConfigEditOutcome_Serialize,
  type OpenConfigFileOutcome,
} from '@/ipc';
import { errorMessage } from '@/utils';

/** The query key of one catalog file's document. */
export const configDocumentKey = (file: string) => ['config', 'document', file] as const;

/** `config_document(file)`: the file's bytes state, hash, value, default and schema. */
export function configDocumentOptions(file: string) {
  return queryOptions({
    queryKey: configDocumentKey(file),
    queryFn: (): Promise<ConfigDocumentOutcome_Serialize> => commands.configDocument(file),
  });
}

/** What a settings field learns from its edit. */
export type EditResult =
  | { status: 'ok' }
  | { status: 'stale' }
  | { status: 'error'; message: string };

/** The message a field shows after a `stale` refusal (its draft is kept). */
export const STALE_MESSAGE =
  'The file changed on disk; your value is kept. Save it again to apply.';

function resultOf(outcome: ConfigEditOutcome_Serialize): EditResult {
  switch (outcome.status) {
    case 'committed':
    case 'unchanged':
      return { status: 'ok' };
    case 'stale':
      return { status: 'stale' };
    case 'invalid':
      return { status: 'error', message: outcome.diagnostic.message };
    default:
      return { status: 'error', message: outcome.message };
  }
}

/**
 * The serialized editor of one file: each edit waits for the previous one,
 * then sends `baseHash`, the hash of the document its op was built from
 * (captured by the caller when the op is built, not when it runs), and
 * refetches the document (also after `stale`) before it resolves. An op
 * queued behind an edit that changed the file is therefore `stale`, not
 * applied to a document it was not built from.
 */
export function createDocumentEditor(client: QueryClient, file: string) {
  let queue: Promise<unknown> = Promise.resolve();

  return (op: ConfigEditOp, baseHash: string | null): Promise<EditResult> => {
    const run = queue.then(async (): Promise<EditResult> => {
      let result: EditResult;

      try {
        result = resultOf(await commands.configEdit({ file, baseHash, op }));
      } catch (error) {
        return { status: 'error', message: errorMessage(error) };
      }

      if (result.status !== 'error') {
        await client.refetchQueries({ queryKey: configDocumentKey(file), exact: true });
      }

      return result;
    });

    queue = run.catch(() => undefined);

    return run;
  };
}

/** `open_config_file(file)`: opens the file with the default app (creating it when missing). */
export function openConfigFileOptions() {
  return mutationOptions({
    mutationKey: ['config', 'open'],
    mutationFn: (file: string): Promise<OpenConfigFileOutcome> => commands.openConfigFile(file),
  });
}

export function useOpenConfigFile() {
  return useMutation(openConfigFileOptions());
}
