import { useEffect, useState } from 'react';

import { Copy01Icon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { events, subscribe } from '@/ipc';
import {
  configDocumentKey,
  configDocumentOptions,
  createDocumentEditor,
  useOpenConfigFile,
} from '@/queries/configDocument';
import { errorMessage } from '@/utils';
import { COPY } from '@/windows/Popover/settingsCopy';
import { styles as controls } from '@/windows/Popover/styles/controls.style';

import { InvalidBanner } from './components/InvalidBanner';
import { SchemaForm } from './components/SchemaForm';
import { Warnings } from './components/Warnings';
import { styles } from './FileTab.style';
import { FormContext } from './utils/form';
import { schemaTree } from './utils/schema';

/**
 * One catalog file's tab: the header (path with copy, Open in editor), the
 * file's state notes (missing, invalid, symlink, warnings) and the generated
 * form. The document is refetched on `config-changed` naming the file and on
 * `popover-shown`; edits go through one serialized editor per file.
 */
export function FileTab({ file }: { file: string }) {
  const client = useQueryClient();
  const query = useQuery(configDocumentOptions(file));
  const [edit] = useState(() => createDocumentEditor(client, file));
  const open = useOpenConfigFile();
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    const refetch = () => {
      void client.invalidateQueries({ queryKey: configDocumentKey(file), exact: true });
    };
    const stops = [
      subscribe(events.configChanged, (payload) => {
        if (payload.files.includes(file)) {
          refetch();
        }
      }),
      subscribe(events.popoverShown, refetch),
    ];

    return () => stops.forEach((stop) => stop());
  }, [client, file]);

  const openInEditor = () => {
    setNotice(null);
    open.mutate(file, {
      onSuccess: (outcome) => {
        if (outcome.status === 'failed') {
          setNotice(`Could not open the file: ${outcome.message}`);
        } else if (outcome.status === 'unknownFile') {
          setNotice('Could not open the file: not a catalog file.');
        }
      },
      onError: (error) => setNotice(`Could not open the file: ${errorMessage(error)}`),
    });
  };

  if (query.data === undefined) {
    return query.isError ? (
      <p {...stylex.props(styles.error)} role="alert">
        Could not read {file}: {errorMessage(query.error)}
      </p>
    ) : (
      <p {...stylex.props(styles.status)} role="status">
        Reading {file}…
      </p>
    );
  }

  if (query.data.status === 'unknownFile') {
    return (
      <p {...stylex.props(styles.error)} role="alert">
        {file} is not a catalog file.
      </p>
    );
  }

  const document = query.data.document;
  const readOnly = document.state === 'invalid' || document.state === 'symlink';
  const copyPath = () => {
    navigator.clipboard.writeText(document.path).then(
      () => setNotice('Path copied.'),
      (error: unknown) => setNotice(`Could not copy the path: ${errorMessage(error)}`),
    );
  };

  return (
    <div {...stylex.props(styles.root)} data-testid="file-tab" data-state={document.state}>
      <div {...stylex.props(styles.header)}>
        <span {...stylex.props(styles.path)} title={document.path}>
          {document.path}
        </span>
        <button
          type="button"
          aria-label="Copy path"
          title="Copy path"
          onClick={copyPath}
          {...stylex.props(controls.round)}
        >
          <Icon icon={Copy01Icon} size={14} />
        </button>
        <button
          type="button"
          disabled={open.isPending}
          onClick={openInEditor}
          {...stylex.props(controls.button, controls.primary)}
        >
          Open in editor
        </button>
      </div>
      {notice !== null ? (
        <p {...stylex.props(styles.note)} role="status">
          {notice}
        </p>
      ) : null}

      <Warnings warnings={document.warnings} />

      {document.state === 'missing' ? (
        <p {...stylex.props(styles.note)} role="status">
          File not created yet; the first change creates it.
        </p>
      ) : null}

      {document.state === 'invalid' ? <InvalidBanner diagnostic={document.diagnostic} /> : null}

      {document.state === 'symlink' ? (
        <p {...stylex.props(styles.note)} role="status">
          This file is a symbolic link, so it is read-only here. Edit it in an editor.
        </p>
      ) : null}

      <FormContext
        value={{
          value: document.value,
          defaults: document.default,
          readOnly,
          // The op is built from this rendered document, so it carries its hash.
          edit: (op) => edit(op, document.hash),
          openInEditor,
        }}
      >
        <SchemaForm tree={schemaTree(document.schema)} copy={COPY[file] ?? {}} />
      </FormContext>
    </div>
  );
}
