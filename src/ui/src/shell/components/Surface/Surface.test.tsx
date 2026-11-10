import { describe, expect, test } from 'vitest';
import { render } from 'vitest-browser-react';

import { motionRaw } from '../../../sdk/tokens';

import { Surface } from './Surface';
import { transition } from './Surface.motion';

describe('Surface', () => {
  test('uses the actual 0.275s slow duration and the legacy easing', () => {
    expect(motionRaw.durationSlow).toBe(0.275);
    expect(transition.duration).toBe(0.275);
    expect(transition.ease).toEqual([0.42, 0, 0.58, 1]);
  });

  test('renders a crust rounded surface and fades in', async () => {
    const screen = await render(<Surface>content</Surface>);
    const element = screen.container.firstElementChild as HTMLElement;
    const style = getComputedStyle(element);

    expect(element.tagName).toBe('DIV');
    expect(style.borderRadius).toBe('12px');
    expect(style.backgroundColor).toBe('rgb(17, 17, 27)');
    expect(style.color).toBe('rgb(205, 214, 244)');
    expect(style.overflow).toBe('hidden');
    expect(style.height).not.toBe('');
    await expect.poll(() => getComputedStyle(element).opacity).toBe('1');
  });

  test('renders another element or component through `as`, merging class names', async () => {
    const screen = await render(
      <Surface as="section" className="extra" animated={false} data-x="1">
        body
      </Surface>,
    );
    const element = screen.container.firstElementChild as HTMLElement;

    expect(element.tagName).toBe('SECTION');
    expect(element.classList.contains('extra')).toBe(true);
    expect(element.dataset.x).toBe('1');
    expect(getComputedStyle(element).opacity).toBe('1');
  });

  test('wraps a custom component', async () => {
    function Custom({ className, children }: { className?: string; children?: React.ReactNode }) {
      return (
        <p className={className} data-custom>
          {children}
        </p>
      );
    }

    const screen = await render(<Surface as={Custom}>custom</Surface>);

    expect(screen.container.querySelector('p[data-custom]')?.textContent).toBe('custom');
  });
});
