import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Renderer } from './Renderer';
import { useRenderer } from './Renderer.state';

vi.mock('@/renderer/Renderer.state', () => ({
  useRenderer: vi.fn(),
}));

vi.mock('@/renderer/bar', () => ({
  Bar: () => <div data-testid="bar-window">Bar</div>,
}));

vi.mock('@/renderer/widgets', () => ({
  Widgets: () => <div data-testid="widgets-window">Widgets</div>,
}));

const mockUseRenderer = vi.mocked(useRenderer);

describe('Renderer', () => {
  test('renders the bar window when the current window is the bar', async () => {
    mockUseRenderer.mockReturnValue({ windowName: 'bar' });

    const screen = await render(<Renderer />);

    await expect.element(screen.getByTestId('bar-window')).toBeVisible();
    await expect.element(screen.getByTestId('widgets-window')).not.toBeInTheDocument();
  });

  test('renders the widgets window when the current window is the widgets window', async () => {
    mockUseRenderer.mockReturnValue({ windowName: 'widgets' });

    const screen = await render(<Renderer />);

    await expect.element(screen.getByTestId('widgets-window')).toBeVisible();
    await expect.element(screen.getByTestId('bar-window')).not.toBeInTheDocument();
  });
});
