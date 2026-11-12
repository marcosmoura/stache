import { DashboardCircleIcon } from '@hugeicons/core-free-icons';
import { MotionGlobalConfig } from 'motion/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Icon } from '@/components/Icon';

import { renderWithSnapshot } from '../../../../../../tests/barSnapshot';
import { barAvailable, tilingStrip } from '../../../../../../tests/fixtures';
import { installReducedMotionQuery, setReducedMotion } from '../../../../../../tests/reducedMotion';
import { render } from '../../../../../../tests/render';

import { Workspace } from './Workspace';

/* Catppuccin surface1 (Mocha, Latte). */
const SURFACE_ACTIVE = /rgb\(69, 71, 90\)|rgb\(188, 192, 204\)/;

installReducedMotionQuery();

afterEach(() => {
  vi.restoreAllMocks();
  MotionGlobalConfig.skipAnimations = true;
  setReducedMotion(false);
});

const outcome = barAvailable('1', { tiling: tilingStrip({ focusedWorkspace: 'terminal' }) });

describe('Workspace', () => {
  it('is an icon-only button named by the workspace, active while focused', async () => {
    const { screen } = await renderWithSnapshot(
      <>
        <Workspace name="terminal" highlightId="h" onFocus={vi.fn()} />
        <Workspace name="coding" highlightId="h" onFocus={vi.fn()} />
      </>,
      outcome,
    );
    const terminal = screen.getByRole('button', { name: 'terminal' });

    await expect.element(terminal).toHaveAttribute('aria-pressed', 'true');
    await expect.element(terminal).toHaveAttribute('data-active', 'true');
    await expect
      .element(screen.getByRole('button', { name: 'coding' }))
      .toHaveAttribute('aria-pressed', 'false');
    expect(terminal.element().textContent).toBe('');

    const indicator = terminal.element().querySelector<HTMLElement>('[data-highlight]')!;
    const style = getComputedStyle(indicator);

    expect(style.position).toBe('absolute');
    expect(style.borderTopLeftRadius).toBe('12px');
    expect(style.backgroundColor).toMatch(SURFACE_ACTIVE);
    expect(getComputedStyle(terminal.element()).paddingLeft).toBe('10px');
    expect(terminal.element().querySelector('svg')!.getBoundingClientRect().width).toBe(18);
  });

  it('sends its name on click', async () => {
    const onFocus = vi.fn();
    const { screen } = await renderWithSnapshot(
      <Workspace name="coding" highlightId="h" onFocus={onFocus} />,
      outcome,
    );

    await screen.getByRole('button', { name: 'coding' }).click();
    expect(onFocus).toHaveBeenCalledWith('coding');
  });

  it('draws the default icon for an unknown name', async () => {
    const { screen } = await renderWithSnapshot(
      <Workspace name="workspace-1" highlightId="h" onFocus={vi.fn()} />,
      outcome,
    );
    const expected = await render(<Icon icon={DashboardCircleIcon} />);
    const button = screen.getByRole('button', { name: 'workspace-1' }).element();

    expect(button.querySelector('svg path')?.getAttribute('d')).toBe(
      expected.container.querySelector('svg path')?.getAttribute('d'),
    );
  });
});

const inactiveStrip = (overrides: Parameters<typeof tilingStrip>[0] = {}) =>
  tilingStrip({
    workspaces: [
      { name: 'terminal', focused: true, inactive: true },
      { name: 'tasks', focused: false, inactive: true },
      { name: 'coding', focused: false, inactive: false },
    ],
    focusedWorkspace: 'terminal',
    ...overrides,
  });

const iconOpacity = (button: Element) => getComputedStyle(button.querySelector('svg')!).opacity;
const flashOf = (name: string) =>
  document.querySelector<HTMLElement>(`button[aria-label="${name}"] > [data-flash]`);
const refused = (workspace: string, serial: number, generation = '3') =>
  barAvailable(String(serial + 10), {
    tiling: inactiveStrip({ generation, refusedFocus: { workspace, serial } }),
  });

describe('Workspace inactive', () => {
  it('is dimmed and aria-disabled, without hover surface; active and focused ones are not', async () => {
    const { screen } = await renderWithSnapshot(
      <>
        <Workspace name="terminal" highlightId="h" onFocus={vi.fn()} />
        <Workspace name="tasks" highlightId="h" onFocus={vi.fn()} />
        <Workspace name="coding" highlightId="h" onFocus={vi.fn()} />
      </>,
      barAvailable('1', { tiling: inactiveStrip() }),
    );
    const tasks = screen.getByRole('button', { name: 'tasks' });

    await expect.element(tasks).toHaveAttribute('aria-disabled', 'true');
    await expect.element(tasks).toHaveAttribute('data-inactive', 'true');
    expect(iconOpacity(tasks.element())).toBe('0.4');
    expect(getComputedStyle(tasks.element()).cursor).toBe('default');
    await tasks.hover();
    await vi.waitFor(() =>
      expect(getComputedStyle(tasks.element()).backgroundColor).toBe('rgba(0, 0, 0, 0)'),
    );

    for (const name of ['terminal', 'coding']) {
      const button = screen.getByRole('button', { name }).element();

      expect(button.hasAttribute('aria-disabled')).toBe(false);
      expect(button.hasAttribute('data-inactive')).toBe(false);
      expect(iconOpacity(button)).toBe('1');
      expect(getComputedStyle(button).cursor).toBe('pointer');
    }
  });

  it('stays focusable and sends its click', async () => {
    const onFocus = vi.fn();
    const { screen } = await renderWithSnapshot(
      <Workspace name="tasks" highlightId="h" onFocus={onFocus} />,
      barAvailable('1', { tiling: inactiveStrip() }),
    );
    const tasks = screen.getByRole('button', { name: 'tasks' });

    (tasks.element() as HTMLButtonElement).focus();
    expect(document.activeElement).toBe(tasks.element());
    // A user's click (Playwright refuses to click an `aria-disabled` element).
    (tasks.element() as HTMLButtonElement).click();
    expect(onFocus).toHaveBeenCalledWith('tasks');
  });
});

describe('Workspace refusal flash', () => {
  /** Samples the tint's opacity and the icon's on every animation frame, until `stop`. */
  function recordFlash(name: string) {
    const seen: { tint: number; icon: string }[] = [];
    let frame = 0;
    const sample = () => {
      const flash = flashOf(name);

      if (flash) {
        seen.push({
          tint: Number.parseFloat(getComputedStyle(flash).opacity),
          icon: iconOpacity(flash.parentElement!),
        });
      }

      frame = requestAnimationFrame(sample);
    };

    frame = requestAnimationFrame(sample);

    return { seen, stop: () => cancelAnimationFrame(frame) };
  }

  const ended = (name: string) =>
    vi.waitFor(
      () => {
        expect(flashOf(name)).toBeNull();
        expect(
          document.querySelector(`[aria-label="${name}"]`)!.hasAttribute('data-flashing'),
        ).toBe(false);
      },
      { timeout: 3000 },
    );

  it('flashes once per new refusal, not for the refusal at first render or another workspace', async () => {
    MotionGlobalConfig.skipAnimations = false;
    const { screen, setSnapshot } = await renderWithSnapshot(
      <>
        <Workspace name="tasks" highlightId="h" onFocus={vi.fn()} />
        <Workspace name="coding" highlightId="h" onFocus={vi.fn()} />
      </>,
      refused('tasks', 1),
    );
    const tasks = screen.getByRole('button', { name: 'tasks' });

    // The refusal present at first render does not flash.
    await expect.element(tasks).toBeVisible();
    await new Promise((done) => setTimeout(done, 100));
    expect(flashOf('tasks')).toBeNull();

    for (const serial of [2, 3]) {
      const frames = recordFlash('tasks');

      try {
        setSnapshot(refused('tasks', serial));
        await vi.waitFor(() => expect(tasks.element().hasAttribute('data-flashing')).toBe(true));
        await ended('tasks');
      } finally {
        frames.stop();
      }

      // The tint faded in (in the attention colour) and the icon was at full opacity.
      expect(frames.seen.some(({ tint }) => tint > 0)).toBe(true);
      expect(frames.seen.every(({ icon }) => icon === '1')).toBe(true);
      expect(iconOpacity(tasks.element())).toBe('0.4');
    }

    // Another workspace's refusal does nothing here.
    const frames = recordFlash('tasks');

    setSnapshot(refused('coding', 4));
    await vi.waitFor(() =>
      expect(
        screen.getByRole('button', { name: 'coding' }).element().hasAttribute('data-flashing'),
      ).toBe(true),
    );
    await ended('coding');
    frames.stop();
    expect(frames.seen).toEqual([]);
    expect(tasks.element().hasAttribute('data-flashing')).toBe(false);
  });

  it('uses the attention colour', async () => {
    setReducedMotion(true);
    const { setSnapshot } = await renderWithSnapshot(
      <Workspace name="tasks" highlightId="h" onFocus={vi.fn()} />,
      barAvailable('1', { tiling: inactiveStrip() }),
    );

    setSnapshot(refused('tasks', 1));
    await vi.waitFor(() => expect(flashOf('tasks')).not.toBeNull());
    // Catppuccin peach (Mocha, Latte).
    expect(getComputedStyle(flashOf('tasks')!).backgroundColor).toMatch(
      /rgb\(250, 179, 135\)|rgb\(254, 100, 11\)/,
    );
  });

  it('under reduced motion the tint shows and goes without interpolating', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(true);
    const { screen, setSnapshot } = await renderWithSnapshot(
      <Workspace name="tasks" highlightId="h" onFocus={vi.fn()} />,
      barAvailable('1', { tiling: inactiveStrip() }),
    );
    const tasks = screen.getByRole('button', { name: 'tasks' });

    await expect.element(tasks).toBeVisible();
    const frames = recordFlash('tasks');

    try {
      setSnapshot(refused('tasks', 1));
      await vi.waitFor(() => expect(flashOf('tasks')).not.toBeNull());
      await ended('tasks');
    } finally {
      frames.stop();
    }

    expect(frames.seen.length).toBeGreaterThan(0);
    expect(frames.seen.every(({ tint, icon }) => tint === 1 && icon === '1')).toBe(true);
  });
});
