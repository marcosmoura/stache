import { afterEach, describe, expect, it, vi } from 'vitest';
import { commands } from 'vitest/browser';

import { recordIpc } from '../../tests/ipc';
import { render } from '../../tests/render';

import { App, AppShell } from './App';

function Throws({ value = new Error('window exploded') }: { value?: unknown }): never {
  throw value;
}

afterEach(async () => {
  await commands.emulateMedia({ colorScheme: null });
});

describe('App', () => {
  it('renders the error boundary fallback when a window throws', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const screen = await render(
      <AppShell>
        <Throws />
      </AppShell>,
    );

    await expect.element(screen.getByRole('alert')).toBeVisible();
    await expect.element(screen.getByRole('heading')).toHaveTextContent('Something went wrong');
    await expect.element(screen.getByText('window exploded')).toBeVisible();
    consoleError.mockRestore();
  });

  it('shows a thrown non-Error value', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const screen = await render(
      <AppShell>
        <Throws value="plain failure" />
      </AppShell>,
    );

    await expect.element(screen.getByText('plain failure')).toBeVisible();
    consoleError.mockRestore();
  });

  it('renders the routed window for a known surface and follows a new search', async () => {
    const screen = await render(<App search="?surface=access" />);

    await expect.element(screen.getByRole('main')).toHaveAttribute('data-surface', 'access');
    await screen.rerender(<App search="?surface=access" />);
    await screen.rerender(<App search="?surface=nope" />);
    await expect.element(screen.getByRole('heading')).toHaveTextContent('Unknown surface');
    await screen.rerender(<App search="?surface=test" />);
    await expect.element(screen.getByRole('main')).toHaveAttribute('data-surface', 'test');
  });

  it('renders the shell surface when the parameter is absent', async () => {
    const screen = await render(<App search="" />);

    await expect.element(screen.getByRole('main')).toHaveTextContent('shell');
  });

  it('renders the local fallback for an unknown surface without IPC', async () => {
    const calls = recordIpc();
    const screen = await render(<App search="?surface=unknown" />);

    await expect.element(screen.getByRole('heading')).toHaveTextContent('Unknown surface');
    expect(calls).toEqual([]);
  });

  it('uses the text token, so the error fallback is readable in dark mode', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    await commands.emulateMedia({ colorScheme: 'dark' });

    const screen = await render(
      <AppShell>
        <Throws />
      </AppShell>,
    );
    const element = screen.getByRole('alert').element();

    // Mocha text (#cdd6f4), not the default black.
    await expect.poll(() => getComputedStyle(element).color).toBe('rgb(205, 214, 244)');
    consoleError.mockRestore();
  });
});
