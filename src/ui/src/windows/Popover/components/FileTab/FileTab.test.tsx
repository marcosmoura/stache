import { describe, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';

import type { ConfigDocument_Serialize, ConfigEditOutcome_Serialize, JsonValue } from '@/ipc';

import { flush, mockHost, type MockHost } from '../../../../../tests/ipc';
import { render } from '../../../../../tests/render';

import { FileTab } from './FileTab';
import { setAt } from './values';

const SCHEMAS = import.meta.glob<JsonValue>('../../../../../../../schemas/*.json', {
  eager: true,
  import: 'default',
});

const schema = (name: string) =>
  Object.entries(SCHEMAS).find(([path]) => path.endsWith(`/${name}.schema.json`))![1];

const TILING_DEFAULT = {
  enabled: false,
  default_layout: 'dwindle',
  gaps: { inner: 0, outer: 0 },
  master: { position: 'auto', ratio: 60 },
  workspaces: [],
  ignore: [],
};

function doc(
  name: string,
  value: JsonValue | null,
  overrides: Partial<ConfigDocument_Serialize> = {},
): ConfigDocument_Serialize {
  return {
    file: `${name}.toml`,
    path: `/Users/me/.config/stache/${name}.toml`,
    state: 'valid',
    hash: 'h1',
    value,
    default: name === 'tiling' ? TILING_DEFAULT : { enabled: false, bindings: {} },
    schema: schema(name),
    diagnostic: null,
    warnings: [],
    ...overrides,
  };
}

type Edit = { file: string; baseHash: string | null; op: unknown };

/** A host serving one document; `onEdit` answers each edit (default: commit with `h2`). */
function setup(
  initial: ConfigDocument_Serialize,
  onEdit: (
    request: Edit,
  ) => ConfigEditOutcome_Serialize | Promise<ConfigEditOutcome_Serialize> = () => committed('h2'),
) {
  const state = { document: initial };
  const host = mockHost()
    .answer('config_document', () => ({ status: 'document', document: state.document }))
    .answer('config_edit', (args: unknown) => onEdit((args as { request: Edit }).request))
    .answer('open_config_file', { status: 'opened' });

  return { host, state };
}

const committed = (hash: string): ConfigEditOutcome_Serialize => ({
  status: 'committed',
  hash,
  reload: { status: 'applied', revision: '2' } as never,
});

const edits = (host: MockHost) =>
  host.callsOf('config_edit').map((call) => (call.args as { request: Edit }).request);

describe('FileTab controls', () => {
  it('a switch writes `set` at once with the current hash', async () => {
    const { host } = setup(doc('keepAwake', {}));
    const screen = await render(<FileTab file="keepAwake.toml" />);

    await expect.element(screen.getByText('default')).toBeVisible();
    await screen.getByRole('switch', { name: 'Enabled' }).click();

    await expect
      .poll(() => edits(host))
      .toEqual([
        {
          file: 'keepAwake.toml',
          baseHash: 'h1',
          op: { op: 'set', path: ['enabled'], value: true },
        },
      ]);
  });

  it('a select opens a menu with descriptions and writes the chosen option', async () => {
    const { host } = setup(doc('tiling', { default_layout: 'dwindle' }));
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'Default layout' }).click();
    await expect
      .element(screen.getByRole('option', { name: /dwindle/ }))
      .toHaveAttribute('aria-selected', 'true');
    await expect.element(screen.getByText('One master window with a stack.')).toBeVisible();
    const selected = screen.getByRole('option', { name: /dwindle/ }).element();

    // A menu: the check leads the selected item, which has the accent highlight.
    expect(selected.firstElementChild?.querySelector('svg')).not.toBeNull();
    expect(getComputedStyle(selected).backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
    await screen.getByRole('option', { name: /^master/ }).click();

    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([{ op: 'set', path: ['default_layout'], value: 'master' }]);
  });

  it('numbers write on Enter, steppers at once, and out-of-range values stay local', async () => {
    const { host, state } = setup(doc('tiling', { master: { ratio: 60 } }), (request) => {
      const op = request.op as { path: string[]; value: JsonValue };

      state.document = {
        ...state.document,
        value: setAt(state.document.value, op.path, op.value) as JsonValue,
      };

      return committed('h2');
    });
    const screen = await render(<FileTab file="tiling.toml" />);
    const ratio = screen.getByRole('textbox', { name: 'Ratio' });
    const stepper = (
      screen.getByRole('button', { name: 'Increase Ratio' }).element() as HTMLElement
    ).parentElement;

    // A text field followed by a small stepper: Increase above Decrease.
    expect(stepper?.previousElementSibling).toBe(ratio.element());
    expect([...(stepper?.children ?? [])].map((child) => child.getAttribute('aria-label'))).toEqual(
      ['Increase Ratio', 'Decrease Ratio'],
    );

    await userEvent.fill(ratio, '-5');
    await userEvent.keyboard('{Enter}');
    await expect.element(screen.getByText('Must be at least 0.')).toBeVisible();
    await expect.element(ratio).toHaveValue('-5');
    expect(edits(host)).toEqual([]);

    await userEvent.fill(ratio, '70');
    await userEvent.keyboard('{Enter}');
    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([{ op: 'set', path: ['master', 'ratio'], value: 70 }]);
    await expect.element(screen.getByRole('button', { name: 'Increase Ratio' })).toBeEnabled();
    await screen.getByRole('button', { name: 'Increase Ratio' }).click();
    await expect.poll(() => edits(host).length).toBe(2);
    expect(edits(host)[1].op).toEqual({ op: 'set', path: ['master', 'ratio'], value: 71 });
  });

  it('text writes on blur and Esc reverts without writing', async () => {
    const { host } = setup(doc('tiling', { workspaces: [{ name: 'coding' }] }));
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'coding', exact: true }).click();
    const name = screen.getByRole('textbox', { name: 'Name' });

    await userEvent.fill(name, 'web');
    await userEvent.keyboard('{Escape}');
    await expect.element(name).toHaveValue('coding');

    await userEvent.fill(name, 'web');
    await userEvent.tab();
    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([{ op: 'set', path: ['workspaces', 0, 'name'], value: 'web' }]);
  });

  it('Enter while a write is in flight sends the draft once, not again on the blur', async () => {
    let release: () => void = () => {};
    const { host } = setup(doc('tiling', { master: { ratio: 60 } }), async () => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });

      return committed('h2');
    });
    const screen = await render(<FileTab file="tiling.toml" />);
    const ratio = screen.getByRole('textbox', { name: 'Ratio' });

    await userEvent.fill(ratio, '70');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => edits(host).length).toBe(1);
    await expect.element(ratio).toBeDisabled();
    (ratio.element() as HTMLInputElement).blur();
    await flush(20);
    release();
    await flush(20);

    expect(edits(host).map((edit) => edit.op)).toEqual([
      { op: 'set', path: ['master', 'ratio'], value: 70 },
    ]);
  });

  it('reset to default sends `remove`', async () => {
    const { host } = setup(doc('tiling', { default_layout: 'master' }));
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'Reset Default layout to default' }).click();
    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([{ op: 'remove', path: ['default_layout'] }]);
  });

  it('maps rename on Enter, remove and add entries', async () => {
    const { host } = setup(doc('keybindings', { bindings: { 'cmd+a': 'open -a Safari' } }));
    const screen = await render(<FileTab file="keybindings.toml" />);
    const entry = screen.getByRole('group', { name: 'cmd+a' });

    await userEvent.fill(entry.getByRole('textbox', { name: 'Key' }), 'cmd+b');
    await userEvent.keyboard('{Enter}');
    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([{ op: 'rename', path: ['bindings'], from: 'cmd+a', to: 'cmd+b' }]);

    await screen.getByRole('button', { name: 'Remove cmd+a' }).click();
    await screen.getByRole('button', { name: 'Add entry' }).click();
    await expect
      .poll(() =>
        edits(host)
          .map((edit) => edit.op)
          .slice(1),
      )
      .toEqual([
        { op: 'remove', path: ['bindings', 'cmd+a'] },
        { op: 'set', path: ['bindings', 'new_key'], value: '' },
      ]);
  });

  it('arrays of tables add, move and remove cards', async () => {
    const { host } = setup(doc('tiling', { workspaces: [{ name: 'a' }, { name: 'b' }] }));
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'Move a down' }).click();
    await expect.poll(() => edits(host).length).toBe(1);
    await screen.getByRole('button', { name: 'Remove b' }).click();
    await expect.poll(() => edits(host).length).toBe(2);
    await screen
      .getByRole('region', { name: 'Workspaces' })
      .getByRole('button', { name: 'Add item' })
      .click();
    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([
        { op: 'move', path: ['workspaces'], from: 0, to: 1 },
        { op: 'remove', path: ['workspaces', 1] },
        { op: 'insert', path: ['workspaces'], index: 2, value: { name: 'new' } },
      ]);
  });

  it('edits to one file are sent one at a time, each with the hash it was built from', async () => {
    let release: () => void = () => {};
    let hash = 1;
    const { host, state } = setup(doc('tiling', {}), async () => {
      if (hash === 1) {
        await new Promise<void>((resolve) => {
          release = resolve;
        });
      }

      hash += 1;
      state.document = { ...state.document, hash: `h${hash}` };

      return committed(`h${hash}`);
    });
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('switch', { name: 'Enabled' }).last().click();
    await expect.poll(() => edits(host).length, { timeout: 5000 }).toBe(1);

    const increase = screen.getByRole('button', { name: 'Increase Ratio' });

    await increase.click();
    // The control locks while its op is queued behind the held edit.
    await expect.element(increase, { timeout: 5000 }).toBeDisabled();
    expect(edits(host)).toHaveLength(1);

    const reads = host.callsOf('config_document').length;

    release();
    // Both ops were built from `h1`; the second must not run on `h2` unchecked.
    await expect
      .poll(() => edits(host).map((edit) => edit.baseHash), { timeout: 5000 })
      .toEqual(['h1', 'h1']);

    // After the refetch, a new op carries the new hash.
    await expect
      .poll(() => host.callsOf('config_document').length, { timeout: 5000 })
      .toBeGreaterThanOrEqual(reads + 2);
    await expect.element(increase, { timeout: 5000 }).toBeEnabled();
    await screen.getByRole('button', { name: 'Decrease Ratio' }).click();
    await expect.poll(() => edits(host).at(-1)?.baseHash, { timeout: 5000 }).toBe('h3');
  });

  it('a rejected invoke for one edit does not block the next edit of the file', async () => {
    let calls = 0;
    const { host } = setup(doc('tiling', {}), () => {
      calls += 1;

      return calls === 1 ? Promise.reject(new Error('ipc broke')) : committed('h2');
    });
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('switch', { name: 'Enabled' }).last().click();
    await expect.element(screen.getByText(/ipc broke/)).toBeVisible();
    await screen.getByRole('button', { name: 'Increase Ratio' }).click();
    await expect
      .poll(() => edits(host).map((edit) => edit.op))
      .toEqual([
        { op: 'set', path: ['enabled'], value: true },
        { op: 'set', path: ['master', 'ratio'], value: 61 },
      ]);
  });
});

/**
 * A host that holds the first edit until released, then commits each edit
 * whose `baseHash` matches the file and answers `stale` otherwise.
 */
function realisticHost(initial: ConfigDocument_Serialize) {
  let release: () => void = () => {};
  let held = true;
  let hash = 1;
  const { host, state } = setup(initial, async (request) => {
    if (held) {
      held = false;
      await new Promise<void>((resolve) => {
        release = resolve;
      });
    }

    if (request.baseHash !== state.document.hash) {
      return { status: 'stale', hash: state.document.hash };
    }

    hash += 1;
    state.document = { ...state.document, hash: `h${hash}` };

    return committed(`h${hash}`);
  });

  return { host, release: () => release() };
}

describe('FileTab superseded queued edits', () => {
  it('a rename queued behind a move in the same card is stale and keeps its draft', async () => {
    const { host, release } = realisticHost(
      doc('tiling', { workspaces: [{ name: 'a' }, { name: 'b' }] }),
    );
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'a', exact: true }).click();
    await screen.getByRole('button', { name: 'Move a down' }).click();
    await expect.poll(() => edits(host).length).toBe(1);
    const name = screen.getByRole('textbox', { name: 'Name' });

    await userEvent.fill(name, 'renamed');
    await userEvent.keyboard('{Enter}');
    release();

    await expect.element(screen.getByText(/The file changed on disk/)).toBeVisible();
    await expect.element(name).toHaveValue('renamed');
    // Every rename send was built from `h1`, so none lands on the moved array.
    expect(edits(host)[0]).toEqual({
      file: 'tiling.toml',
      baseHash: 'h1',
      op: { op: 'move', path: ['workspaces'], from: 0, to: 1 },
    });
    const renames = edits(host).slice(1);

    expect(renames.length).toBeGreaterThan(0);
    expect(renames.every((edit) => edit.baseHash === 'h1')).toBe(true);
    expect(renames[0]?.op).toEqual({
      op: 'set',
      path: ['workspaces', 0, 'name'],
      value: 'renamed',
    });
  });

  it('a second whole-array write from defaults queued behind the first is stale', async () => {
    const { host, release } = realisticHost(
      doc(
        'tiling',
        {},
        {
          default: { ...TILING_DEFAULT, workspaces: [{ name: 'a' }, { name: 'b' }] },
        },
      ),
    );
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'b', exact: true }).click();
    await screen.getByRole('button', { name: 'Move a down' }).click();
    await expect.poll(() => edits(host).length).toBe(1);
    const name = screen.getByRole('textbox', { name: 'Name' });

    await userEvent.fill(name, 'z');
    await userEvent.keyboard('{Enter}');
    release();

    await expect.element(screen.getByText(/The file changed on disk/)).toBeVisible();
    await expect.element(name).toHaveValue('z');
    // Both are whole-array writes from defaults built from `h1`; the second
    // would otherwise drop the move.
    expect(
      edits(host)
        .map((edit) => [edit.op, edit.baseHash])
        .slice(0, 2),
    ).toEqual([
      [{ op: 'set', path: ['workspaces'], value: [{ name: 'b' }, { name: 'a' }] }, 'h1'],
      [{ op: 'set', path: ['workspaces'], value: [{ name: 'a' }, { name: 'z' }] }, 'h1'],
    ]);
    expect(
      edits(host)
        .slice(1)
        .every((edit) => edit.baseHash === 'h1'),
    ).toBe(true);
  });
});

describe('FileTab refusals and sync', () => {
  it('a validation failure shows inline and keeps the draft', async () => {
    setup(doc('tiling', { workspaces: [{ name: 'a' }] }), () => ({
      status: 'invalid',
      diagnostic: {
        message: 'duplicate workspace name',
        line: 4,
        column: 1,
        field: 'workspaces[0].name',
      },
    }));
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'a', exact: true }).click();
    const name = screen.getByRole('textbox', { name: 'Name' });

    await userEvent.fill(name, 'b');
    await userEvent.keyboard('{Enter}');
    await expect.element(screen.getByText('duplicate workspace name')).toBeVisible();
    await expect.element(name).toHaveValue('b');
  });

  it('`stale` refetches and keeps the draft', async () => {
    const { host, state } = setup(doc('tiling', { workspaces: [{ name: 'a' }] }), () => {
      state.document = { ...state.document, hash: 'h9' };

      return { status: 'stale', hash: 'h9' };
    });
    const screen = await render(<FileTab file="tiling.toml" />);

    await screen.getByRole('button', { name: 'a', exact: true }).click();
    const name = screen.getByRole('textbox', { name: 'Name' });

    await userEvent.fill(name, 'b');
    await userEvent.keyboard('{Enter}');
    await expect.element(screen.getByText(/The file changed on disk/)).toBeVisible();
    await expect.element(name).toHaveValue('b');
    expect(host.callsOf('config_document')).toHaveLength(2);
  });

  it('an external change updates unfocused fields and flags a focused one', async () => {
    const { host, state } = setup(
      doc('tiling', { master: { ratio: 60 }, workspaces: [{ name: 'a' }] }),
    );
    const screen = await render(<FileTab file="tiling.toml" />);
    const ratio = screen.getByRole('textbox', { name: 'Ratio' });

    await screen.getByRole('button', { name: 'a', exact: true }).click();
    const name = screen.getByRole('textbox', { name: 'Name' });

    await userEvent.fill(name, 'draft');
    state.document = {
      ...state.document,
      hash: 'h3',
      value: { master: { ratio: 40 }, workspaces: [{ name: 'disk' }] },
    };
    host.emit('config-changed', { files: ['bar.toml'] });
    await flush();
    expect(host.callsOf('config_document')).toHaveLength(1);

    host.emit('config-changed', { files: ['tiling.toml'] });
    await expect.element(ratio).toHaveValue('40');
    await expect.element(name).toHaveValue('draft');
    await expect.element(screen.getByText('Changed on disk')).toBeVisible();

    await screen.getByRole('button', { name: 'Use disk value' }).click();
    await expect.element(screen.getByRole('textbox', { name: 'Name' })).toHaveValue('disk');
    expect(edits(host)).toEqual([]);
  });

  it('refetches when the popover is shown', async () => {
    const { host } = setup(doc('keepAwake', {}));
    const screen = await render(<FileTab file="keepAwake.toml" />);

    await expect.element(screen.getByRole('switch', { name: 'Enabled' })).toBeVisible();
    host.emit('popover-shown', null);
    await expect.poll(() => host.callsOf('config_document').length).toBe(2);
  });
});

describe('FileTab states', () => {
  it('a missing file shows defaults and the creation note', async () => {
    setup(doc('keepAwake', null, { state: 'missing', hash: null }));
    const screen = await render(<FileTab file="keepAwake.toml" />);

    await expect.element(screen.getByText(/File not created yet/)).toBeVisible();
    await expect.element(screen.getByRole('switch', { name: 'Enabled' })).not.toBeChecked();
    await expect.element(screen.getByRole('switch', { name: 'Enabled' })).toBeEnabled();
  });

  it('an invalid file shows the diagnostic, is read-only and opens in the editor', async () => {
    const { host } = setup(
      doc('tiling', null, {
        state: 'invalid',
        diagnostic: { message: 'expected `=`', line: 3, column: 7, field: null },
      }),
    );
    const screen = await render(<FileTab file="tiling.toml" />);

    await expect.element(screen.getByText('Line 3, column 7: expected `=`')).toBeVisible();
    await expect.element(screen.getByRole('switch', { name: 'Enabled' }).last()).toBeDisabled();
    await expect.element(screen.getByRole('button', { name: 'Default layout' })).toBeDisabled();
    await screen.getByRole('button', { name: 'Open in editor' }).click();
    await expect.poll(() => host.callsOf('open_config_file')).toHaveLength(1);
  });

  it('a symlinked file is read-only with a note', async () => {
    setup(doc('keepAwake', { enabled: true }, { state: 'symlink' }));
    const screen = await render(<FileTab file="keepAwake.toml" />);

    await expect.element(screen.getByText(/symbolic link/)).toBeVisible();
    await expect.element(screen.getByRole('switch', { name: 'Enabled' })).toBeDisabled();
    expect(screen.container.textContent).not.toContain('Reset');
  });

  it('lists warnings at the top and shows the path', async () => {
    setup(doc('keepAwake', {}, { warnings: ['unknown key `foo`'] }));
    const screen = await render(<FileTab file="keepAwake.toml" />);

    const warnings = screen.getByRole('region', { name: 'Warnings' });

    await expect.element(warnings).toBeVisible();
    expect(warnings.element().textContent).toContain('unknown key `foo`');
    await expect
      .element(screen.getByTitle('/Users/me/.config/stache/keepAwake.toml'))
      .toBeVisible();
  });
});
