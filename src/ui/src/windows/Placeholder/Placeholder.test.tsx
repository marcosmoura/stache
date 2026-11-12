import { describe, expect, it } from 'vitest';

import { render } from '../../../tests/render';

import { Placeholder } from './Placeholder';

describe('Placeholder', () => {
  it('renders the surface name', async () => {
    const screen = await render(<Placeholder surface="bar" />);

    await expect.element(screen.getByRole('main')).toHaveTextContent('bar');
  });

  it('follows a surface change and keeps its output on an identical render', async () => {
    const screen = await render(<Placeholder surface="bar" />);

    await screen.rerender(<Placeholder surface="bar" />);
    await expect.element(screen.getByRole('main')).toHaveTextContent('bar');
    await screen.rerender(<Placeholder surface="test" />);
    await expect.element(screen.getByRole('main')).toHaveTextContent('test');
  });

  it('applies its StyleX rules', async () => {
    const screen = await render(<Placeholder surface="shell" />);
    const element = screen.getByRole('main').element();

    await expect.poll(() => getComputedStyle(element).display).toBe('grid');
    expect(getComputedStyle(element).placeItems).toBe('center');
  });
});
