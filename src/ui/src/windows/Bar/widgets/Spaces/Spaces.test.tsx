import type { ComponentProps } from 'react';

import { MotionGlobalConfig } from 'motion/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import type { StripApp, TilingStripView_Serialize } from '@/ipc';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import {
  barAvailable,
  barUnavailable,
  tilingStrip,
  USER_WORKSPACES,
} from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';
import { installReducedMotionQuery, setReducedMotion } from '../../../../../tests/reducedMotion';

import { WORKSPACE_ICONS } from './icons';
import { Spaces } from './Spaces';

const renders = vi.hoisted(() => ({
  app: new Map<string, number>(),
  workspace: new Map<string, number>(),
}));

vi.mock('./App/App', async (importOriginal) => {
  const original = await importOriginal<typeof import('./App/App')>();

  return {
    ...original,
    App: (props: ComponentProps<typeof original.App>) => {
      renders.app.set(props.windowKey, (renders.app.get(props.windowKey) ?? 0) + 1);

      // Called as a function, so the original's hooks (and their re-renders) are counted here.
      return original.App(props);
    },
  };
});

vi.mock('./Workspace/Workspace', async (importOriginal) => {
  const original = await importOriginal<typeof import('./Workspace/Workspace')>();

  return {
    ...original,
    Workspace: (props: ComponentProps<typeof original.Workspace>) => {
      renders.workspace.set(props.name, (renders.workspace.get(props.name) ?? 0) + 1);

      return original.Workspace(props);
    },
  };
});

installReducedMotionQuery();

afterEach(async () => {
  vi.restoreAllMocks();
  MotionGlobalConfig.skipAnimations = true;
  setReducedMotion(false);
  await page.viewport(414, 896);
});

const app = (key: string, appName: string, title = ''): StripApp => ({ key, appName, title });

const widget = <Spaces id="spaces" zone="start" options={{}} />;

const renderStrip = (tiling: TilingStripView_Serialize | null) =>
  renderWithSnapshot(widget, barAvailable('1', { tiling }));

const workspaceNames = () =>
  [...document.querySelectorAll('[data-spaces] [role="group"] > button')].map((button) =>
    button.getAttribute('aria-label'),
  );
const appButtons = () => [...document.querySelectorAll<HTMLElement>('[data-app]')];
const appKeys = () => appButtons().map((item) => item.dataset.app);
const label = () => document.querySelector<HTMLElement>('[data-label]');
const highlight = (workspace?: string) =>
  document.querySelector<HTMLElement>(
    workspace ? `button[aria-label="${workspace}"] > [data-highlight]` : '[data-highlight]',
  );
const labelOf = (key: string) =>
  document.querySelector<HTMLElement>(`[data-app="${key}"] [data-label]`);
const settle = (ms = 30) => new Promise((done) => setTimeout(done, ms));

describe('Spaces states', () => {
  it.each([
    ['null', null],
    ['absent', { kind: 'absent' } as const],
    ['zero workspaces', tilingStrip({ workspaces: [] })],
  ])('%s renders nothing', async (_label, tiling) => {
    mockHost();
    const { screen } = await renderStrip(tiling);

    await settle();
    expect(screen.container.childElementCount).toBe(0);
  });

  it('renders nothing while the Bar is not available', async () => {
    mockHost();
    const { screen } = await renderWithSnapshot(widget, barUnavailable('1'));

    await settle();
    expect(screen.container.childElementCount).toBe(0);
  });

  it('loading renders a Loading... item', async () => {
    mockHost();
    const { screen } = await renderStrip({ kind: 'loading' });

    await expect.element(screen.getByText('Loading...')).toBeVisible();
    expect(screen.container.querySelectorAll('button')).toHaveLength(0);
  });

  it('available renders the workspaces in published order, then the apps, filling the Bar height', async () => {
    mockHost();
    await page.viewport(2400, 200);
    const { screen } = await renderWithSnapshot(
      <div style={{ display: 'flex', height: 32 }}>{widget}</div>,
      barAvailable('1', {
        tiling: tilingStrip({
          workspaces: ['zeta', 'files', 'Alpha', 'coding', 'terminal'].map((name) => ({
            name,
            focused: name === 'coding',
            inactive: false,
          })),
          apps: [app('1', 'Ghostty'), app('2', 'Code', 'a.ts'), app('3', 'Safari')],
          focusedApp: app('2', 'Code', 'a.ts'),
          focusedWorkspace: 'coding',
        }),
      }),
    );

    await expect.element(screen.getByRole('group', { name: 'Workspaces' })).toBeVisible();
    // Tiling's order, unsorted; unknown names keep their place.
    expect(workspaceNames()).toEqual(['zeta', 'files', 'Alpha', 'coding', 'terminal']);
    expect(appKeys()).toEqual(['1', '2', '3']);
    await expect
      .element(screen.getByRole('button', { name: 'coding' }))
      .toHaveAttribute('aria-pressed', 'true');
    await expect
      .element(screen.getByRole('button', { name: 'terminal' }))
      .toHaveAttribute('aria-pressed', 'false');

    const row = document.querySelector<HTMLElement>('[data-spaces]')!;
    const items = [row.firstElementChild!, ...row.querySelectorAll('[data-app] > button')];

    await vi.waitFor(() =>
      expect(items.map((item) => item.getBoundingClientRect().height)).toEqual([32, 32, 32, 32]),
    );
    expect(getComputedStyle(row).columnGap).toBe('4px');
    // The focused app shows its label; the others are icon-only, named by their label.
    await vi.waitFor(() => expect(label()?.textContent).toBe('Code'));
    await expect.element(screen.getByRole('button', { name: 'Ghostty' })).toBeVisible();
    expect(row.querySelectorAll('svg')).toHaveLength(8);
  });

  it("renders the user's workspaces in their configured order, each with its icon", async () => {
    mockHost();
    const { screen } = await renderStrip(tilingStrip());

    await expect.element(screen.getByRole('group', { name: 'Workspaces' })).toBeVisible();
    expect(workspaceNames()).toEqual(USER_WORKSPACES);

    for (const name of USER_WORKSPACES) {
      expect(Object.hasOwn(WORKSPACE_ICONS, name), name).toBe(true);
    }
  });

  it('switches between states', async () => {
    mockHost();
    const { screen, setView } = await renderStrip({ kind: 'loading' });

    await expect.element(screen.getByText('Loading...')).toBeVisible();
    setView({ tiling: tilingStrip() });
    await expect.element(screen.getByRole('group', { name: 'Workspaces' })).toBeVisible();
    setView({ tiling: { kind: 'absent' } });
    await vi.waitFor(() => expect(screen.container.childElementCount).toBe(0));
  });
});

describe('Spaces labels', () => {
  const long = 'An extremely long window title that keeps on going forever.ts';
  const apps = [app('1', 'Code', long), app('2', 'Code', 'b.ts'), app('3', 'Code', '')];

  it.each([
    [2400, `${long.slice(0, 40)}...`],
    [2036, 'Code'],
  ])('at width %i the focused label is %j', async (width, expected) => {
    mockHost();
    await page.viewport(width, 200);
    await renderStrip(tilingStrip({ apps, focusedApp: apps[0] }));

    await vi.waitFor(() => expect(label()?.textContent).toBe(expected));
  });

  it('labels a window title when its app has two windows, and the app name for an empty title', async () => {
    mockHost();
    await page.viewport(2400, 200);
    const { setView } = await renderStrip(tilingStrip({ apps, focusedApp: apps[1] }));

    await vi.waitFor(() => expect(label()?.textContent).toBe('b.ts'));
    setView({ tiling: tilingStrip({ apps, focusedApp: apps[2] }) });
    await vi.waitFor(() => expect(label()?.textContent).toBe('Code'));
  });

  it('lists only the focused app in compact mode', async () => {
    mockHost();
    await page.viewport(2036, 200);
    const { setView } = await renderStrip(tilingStrip({ apps, focusedApp: apps[1] }));

    await vi.waitFor(() => expect(appKeys()).toEqual(['2']));
    await page.viewport(2037, 200);
    await vi.waitFor(() => expect(appKeys()).toEqual(['1', '2', '3']));
    await vi.waitFor(() => expect(label()?.textContent).toBe('b.ts'));
    setView({ tiling: tilingStrip({ apps, focusedApp: null }) });
    await vi.waitFor(() => expect(label()).toBeNull());
  });
});

describe('Spaces clicks', () => {
  const tilingHost = () => mockHost().answer('bar_tiling_action', { kind: 'applied' });

  it('a workspace click sends focusWorkspace with the rendered generation and sequence', async () => {
    const host = tilingHost();
    const { screen } = await renderStrip(
      tilingStrip({ generation: '12', sequence: '18446744073709551615' }),
    );

    await screen.getByRole('button', { name: 'coding' }).click();
    await vi.waitFor(() => expect(host.callsOf('bar_tiling_action')).toHaveLength(1));
    expect(host.callsOf('bar_tiling_action')[0]!.args).toEqual({
      request: {
        generation: '12',
        sequence: '18446744073709551615',
        action: { kind: 'focusWorkspace', name: 'coding' },
      },
    });
  });

  it('an inactive workspace click sends the same focusWorkspace request', async () => {
    const host = mockHost().answer('bar_tiling_action', { kind: 'refused' });
    const { screen } = await renderStrip(
      tilingStrip({
        generation: '5',
        sequence: '9',
        workspaces: USER_WORKSPACES.map((name) => ({
          name,
          focused: name === 'terminal',
          inactive: name === 'tasks' || name === 'misc',
        })),
      }),
    );
    const tasks = screen.getByRole('button', { name: 'tasks' });

    await expect.element(tasks).toHaveAttribute('aria-disabled', 'true');
    // A user's click (Playwright refuses to click an `aria-disabled` element).
    (tasks.element() as HTMLButtonElement).click();
    await vi.waitFor(() => expect(host.callsOf('bar_tiling_action')).toHaveLength(1));
    await settle();
    expect(host.callsOf('bar_tiling_action')).toHaveLength(1);
    expect(host.callsOf('bar_tiling_action')[0]!.args).toEqual({
      request: {
        generation: '5',
        sequence: '9',
        action: { kind: 'focusWorkspace', name: 'tasks' },
      },
    });
  });

  it('an app click sends focusWindow with the window key', async () => {
    const host = tilingHost();

    await page.viewport(2400, 200);
    const { screen } = await renderStrip(
      tilingStrip({ apps: [app('100', 'Ghostty'), app('205', 'Code')] }),
    );

    await screen.getByRole('button', { name: 'Code' }).click();
    await vi.waitFor(() => expect(host.callsOf('bar_tiling_action')).toHaveLength(1));
    expect(host.callsOf('bar_tiling_action')[0]!.args).toEqual({
      request: { generation: '3', sequence: '7', action: { kind: 'focusWindow', window: '205' } },
    });
  });

  it('after a newer strip renders, a click carries its generation and sequence', async () => {
    const host = tilingHost();
    const { screen, setView } = await renderStrip(tilingStrip());

    await expect.element(screen.getByRole('button', { name: 'terminal' })).toBeVisible();
    setView({ tiling: tilingStrip({ generation: '4', sequence: '1' }) });
    await screen.getByRole('button', { name: 'terminal' }).click();
    await vi.waitFor(() => expect(host.callsOf('bar_tiling_action')).toHaveLength(1));
    expect(host.callsOf('bar_tiling_action')[0]!.args).toEqual({
      request: {
        generation: '4',
        sequence: '1',
        action: { kind: 'focusWorkspace', name: 'terminal' },
      },
    });
  });

  it.each([
    ['loading', { kind: 'loading' } as const],
    ['absent', { kind: 'absent' } as const],
    ['null', null],
  ])('sends nothing once the strip turned %s', async (_label, tiling) => {
    const host = tilingHost();
    const { screen, setView } = await renderStrip(tilingStrip());
    const button = screen.getByRole('button', { name: 'coding' }).element() as HTMLButtonElement;

    // The button is still in hand when the strip changes under it.
    setView({ tiling });
    button.click();
    await settle();
    expect(host.callsOf('bar_tiling_action')).toHaveLength(0);
  });
});

/**
 * Records `read()` on every insertion and inline style change in the document
 * (each animation frame Motion writes), until `stop`. Nothing runs after `stop`.
 */
function recordFrames<T>(read: () => T | undefined) {
  const seen: T[] = [];
  const observer = new MutationObserver(() => {
    const value = read();

    if (value !== undefined) {
      seen.push(value);
    }
  });

  observer.observe(document.body, {
    attributes: true,
    attributeFilter: ['style'],
    childList: true,
    subtree: true,
  });

  return { seen, stop: () => observer.disconnect() };
}

const px = (value: string | undefined) => Number.parseFloat(value ?? '');

describe('Spaces motion', () => {
  it('moves the highlight to the newly focused workspace with a spring', async () => {
    MotionGlobalConfig.skipAnimations = false;
    mockHost();
    const { setView } = await renderStrip(tilingStrip());

    await vi.waitFor(() => expect(highlight('terminal')).not.toBeNull());
    await settle(100);

    const frames = recordFrames(() => highlight('coding')?.style.transform);

    try {
      setView({ tiling: tilingStrip({ focusedWorkspace: 'coding' }) });
      await vi.waitFor(
        () => {
          expect(document.querySelectorAll('[data-highlight]')).toHaveLength(1);
          expect(highlight('coding')!.style.transform).toMatch(/^(none|)$/);
        },
        { timeout: 2000 },
      );
    } finally {
      frames.stop();
    }

    // It started from the old workspace (translated) and settled in place.
    expect(frames.seen.some((transform) => /translate/.test(transform))).toBe(true);
  });

  it('reveals the focused label by width and opacity, and animates apps in and out', async () => {
    MotionGlobalConfig.skipAnimations = false;
    mockHost();
    await page.viewport(2400, 200);
    // Long names: the reveal lasts length × 10 ms, so frames are sampled even on a busy machine.
    const apps = [app('1', 'G'.repeat(60)), app('2', 'A rather long application name'.repeat(2))];
    const { setView } = await renderStrip(tilingStrip({ apps, focusedApp: apps[0] }));

    await vi.waitFor(() => expect(labelOf('1')?.style.width).toBe('auto'), { timeout: 2000 });

    const reveal = recordFrames(() => {
      const label = labelOf('2');

      return label ? { width: label.style.width, opacity: label.style.opacity } : undefined;
    });

    try {
      setView({ tiling: tilingStrip({ apps, focusedApp: apps[1] }) });
      await vi.waitFor(
        () => {
          expect(labelOf('2')!.style.width).toBe('auto');
          expect(labelOf('1')).toBeNull();
        },
        { timeout: 2000 },
      );
    } finally {
      reveal.stop();
    }

    // The label is inserted at width 0 and transparent and ends at its full
    // width. (Intermediate frames are not asserted: on a busy machine the first
    // frame can already be past the end; the duration is tested in Spaces.state.)
    expect(reveal.seen[0]).toEqual({ width: '0px', opacity: '0' });
    expect(reveal.seen.at(-1)?.width).toBe('auto');
    expect(getComputedStyle(labelOf('2')!).opacity).toBe('1');

    // A new window enters from width 0.
    const entering = recordFrames(() => appButtons()[2]?.style.width);

    try {
      setView({
        tiling: tilingStrip({ apps: [...apps, app('3', 'Safari')], focusedApp: apps[1] }),
      });
      await vi.waitFor(() => expect(appButtons()[2]?.style.width).toBe('auto'), { timeout: 2000 });
    } finally {
      entering.stop();
    }

    expect(entering.seen.some((width) => px(width) >= 0 && px(width) < 30)).toBe(true);

    // A closed window stays while it leaves, then goes.
    const leaving = recordFrames(() =>
      appKeys().includes('1') ? appButtons()[0]!.style.width : undefined,
    );

    try {
      setView({ tiling: tilingStrip({ apps: [apps[1]!], focusedApp: apps[1] }) });
      await vi.waitFor(() => expect(appKeys()).toEqual(['2']), { timeout: 2000 });
    } finally {
      leaving.stop();
    }

    // Motion wrote its exit width while the item was still there.
    expect(leaving.seen.some((width) => width !== 'auto')).toBe(true);
  });

  it('dims apps that are not focused', async () => {
    mockHost();
    await page.viewport(2400, 200);
    const apps = [app('1', 'Ghostty'), app('2', 'Code')];

    await renderStrip(tilingStrip({ apps, focusedApp: apps[0] }));
    await vi.waitFor(() =>
      expect(appButtons().map((item) => item.style.opacity)).toEqual(['1', '0.68']),
    );
  });

  it('is instant under reduced motion', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(true);
    mockHost();
    await page.viewport(2400, 200);
    const apps = [app('1', 'Ghostty'), app('2', 'Code')];
    const { setView } = await renderStrip(tilingStrip({ apps, focusedApp: apps[0] }));

    await vi.waitFor(() => expect(labelOf('1')?.style.width).toBe('auto'));
    setView({
      tiling: tilingStrip({ apps: [apps[1]!], focusedApp: apps[1], focusedWorkspace: 'coding' }),
    });
    await vi.waitFor(
      () => {
        expect(appKeys()).toEqual(['2']);
        expect(labelOf('2')?.style.width).toBe('auto');
        expect(document.querySelectorAll('[data-highlight]')).toHaveLength(1);
        // The spring takes 500 ms; reduced, the highlight is in place within a frame or two.
        expect(highlight('coding')?.style.transform).toMatch(/^(none|)$/);
      },
      { timeout: 150, interval: 5 },
    );
    expect(getComputedStyle(labelOf('2')!).opacity).toBe('1');
  });
});

describe('Spaces render cost', () => {
  it('a focus change re-renders only the items it touches; a frame-only change re-renders nothing', async () => {
    mockHost();
    await page.viewport(2400, 200);
    const apps = [app('1', 'Ghostty'), app('2', 'Code'), app('3', 'Safari')];
    const base = {
      workspaces: ['terminal', 'coding', 'browser'].map((name) => ({
        name,
        focused: false,
        inactive: false,
      })),
      apps,
      focusedApp: apps[0],
    };
    const tiling = tilingStrip(base);
    const { setView } = await renderStrip(tiling);

    await vi.waitFor(() => expect(label()?.textContent).toBe('Ghostty'));
    await settle(50);
    renders.app.clear();
    renders.workspace.clear();

    // Another reading changes (a CPU sample): the strip is the same.
    setView({ tiling, cpu: { usage: 42, temperature: null } });
    await settle(50);
    expect([...renders.app.values(), ...renders.workspace.values()]).toEqual([]);

    // The focused app moves from Ghostty to Code; the workspace stays.
    setView({ tiling: tilingStrip({ ...base, sequence: '8', focusedApp: apps[1] }) });
    await vi.waitFor(() => expect(label()?.textContent).toBe('Code'));
    await settle(50);
    expect(renders.workspace.size).toBe(0);
    expect(renders.app.has('3')).toBe(false);
    expect([...renders.app.keys()].sort()).toEqual(['1', '2']);
    renders.app.clear();

    // The focused workspace moves: only the two workspaces involved.
    setView({
      tiling: tilingStrip({
        ...base,
        sequence: '9',
        focusedApp: apps[1],
        focusedWorkspace: 'coding',
      }),
    });
    await vi.waitFor(() => expect(highlight('coding')).not.toBeNull());
    await settle(50);
    expect(renders.app.size).toBe(0);
    expect([...renders.workspace.keys()].sort()).toEqual(['coding', 'terminal']);
  });

  it('an inactive change or a refusal re-renders only the workspace it touches', async () => {
    mockHost();
    const workspaces = (inactive: string[]) =>
      ['terminal', 'coding', 'tasks'].map((name) => ({
        name,
        focused: name === 'terminal',
        inactive: inactive.includes(name),
      }));
    const { setView } = await renderStrip(tilingStrip({ workspaces: workspaces([]) }));
    const tasks = () => document.querySelector<HTMLElement>('button[aria-label="tasks"]')!;

    await vi.waitFor(() => expect(highlight('terminal')).not.toBeNull());
    await settle(50);
    renders.app.clear();
    renders.workspace.clear();

    setView({ tiling: tilingStrip({ sequence: '8', workspaces: workspaces(['tasks']) }) });
    await vi.waitFor(() => expect(tasks().dataset.inactive).toBe('true'));
    await settle(50);
    expect(renders.app.size).toBe(0);
    expect([...renders.workspace.keys()]).toEqual(['tasks']);
    renders.workspace.clear();

    setView({
      tiling: tilingStrip({
        sequence: '9',
        workspaces: workspaces(['tasks']),
        refusedFocus: { workspace: 'tasks', serial: 1 },
      }),
    });
    await settle(100);
    expect(renders.app.size).toBe(0);
    expect([...renders.workspace.keys()]).toEqual(['tasks']);
  });
});
