import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Icon } from '../../../../../components/Icon';
import { appIcons, getAppIcon } from '../../Spaces.constants';

import { App } from './App';

describe('App Component', () => {
  test('renders app with icon', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId="100"
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(container.querySelector('button')).toBeDefined();
      expect(container.querySelector('svg')).toBeDefined();
    });
  });

  test('renders display name when focused', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId="100"
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(getByText('Ghostty')).toBeDefined();
    });
  });

  test('does not render display name when not focused', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId="100"
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(container.textContent).not.toContain('Ghostty');
    });
  });

  test('calls onClick when clicked', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId="100"
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(async () => {
      const button = container.querySelector('button');

      expect(button).toBeDefined();
      button?.click();
      expect(onClick).toHaveBeenCalledTimes(1);
    });
  });

  test('applies focused styles when focused', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId="100"
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      // When focused, display name should be visible
      expect(getByText('Ghostty')).toBeDefined();
    });
  });

  test('does not show display name when not focused', async () => {
    const onClick = vi.fn();
    const { container } = await render(
      <App
        appName="Ghostty"
        displayName="Ghostty"
        windowId="100"
        isFocused={false}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      // When not focused, display name should not be visible
      expect(container.textContent).not.toContain('Ghostty');
    });
  });

  test('renders window title as display name when provided', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Code"
        displayName="index.ts"
        windowId="100"
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(getByText('index.ts')).toBeDefined();
    });
  });

  test('renders truncated display name', async () => {
    const onClick = vi.fn();
    const { getByText } = await render(
      <App
        appName="Code"
        displayName="This is a very long ..."
        windowId="100"
        isFocused={true}
        onClick={onClick}
      />,
    );

    await vi.waitFor(() => {
      expect(getByText('This is a very long ...')).toBeDefined();
    });
  });
});

describe('App icons, styles and motion', () => {
  test.each([...Object.keys(appIcons), 'Unknown App'])(
    'renders the mapped icon for %j',
    async (appName) => {
      const { container: expected } = await render(<Icon icon={getAppIcon(appName)} />);
      const { container } = await render(
        <App
          appName={appName}
          displayName={appName}
          windowId="1"
          isFocused={false}
          onClick={vi.fn()}
        />,
      );

      expect(container.querySelector('button')?.innerHTML).toBe(expected.innerHTML);
    },
  );

  test('focused and unfocused legacy styles', async () => {
    const { container } = await render(
      <>
        <App appName="Code" displayName="Code" windowId="1" isFocused onClick={vi.fn()} />
        <App appName="Zed" displayName="Zed" windowId="2" isFocused={false} onClick={vi.fn()} />
      </>,
    );
    const [focused, other] = container.querySelectorAll('button');

    if (!focused || !other) {
      throw new Error('buttons missing');
    }

    await vi.waitFor(() => expect(getComputedStyle(focused).opacity).toBe('1'));
    expect(getComputedStyle(focused).backgroundColor).toBe('rgb(17, 17, 27)');
    expect(getComputedStyle(other).opacity).toBe('0.68');
    expect(getComputedStyle(focused).columnGap).toBe('6px');
    expect(getComputedStyle(focused).paddingLeft).toBe('10px');
    expect(getComputedStyle(focused).transitionProperty).toBe('background-color, opacity');
    expect(getComputedStyle(focused).transitionDuration).toBe('0.15s');
  });

  test('motion is not reduced (legacy has no MotionConfig): the reveal starts at width 0 / opacity 0 and settles at auto / 1', async () => {
    const { container } = await render(
      <App appName="Code" displayName="Code" windowId="1" isFocused onClick={vi.fn()} />,
    );
    const outer = container.firstElementChild as HTMLElement;
    const label = outer.querySelector('button > div') as HTMLElement;

    expect(outer.style.opacity).toBe('0');
    expect(outer.style.width).toBe('0px');
    expect(label.style.opacity).toBe('0');
    await vi.waitFor(() => {
      expect(outer.style.opacity).toBe('1');
      expect(outer.style.width).toBe('auto');
      expect(label.style.opacity).toBe('1');
    });
    expect(getComputedStyle(label).whiteSpace).toBe('nowrap');
    expect(getComputedStyle(label).overflow).toBe('hidden');
  });
});
