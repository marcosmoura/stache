import { describe, expect, it } from 'vitest';

import type { ModuleStatusView_Serialize } from '@/ipc';

import { render } from '../../../../../tests/render';

import { ModuleList } from './ModuleList';

const module = (
  overrides: Partial<ModuleStatusView_Serialize> = {},
): ModuleStatusView_Serialize => ({
  id: 'proxyAudio',
  state: 'disabled',
  enabled: false,
  paused: false,
  revision: '3',
  detail: null,
  diagnostic: null,
  ...overrides,
});

const row = (card: Element, label: string) =>
  Array.from(card.querySelectorAll('div'))
    .find((candidate) => candidate.querySelector(':scope > dt')?.textContent === label)
    ?.querySelector(':scope > dd')?.textContent;

describe('ModuleList', () => {
  it('says when no module is registered', async () => {
    const screen = await render(<ModuleList modules={[]} />);

    await expect.element(screen.getByText('No modules registered.')).toBeVisible();
  });

  it('renders a card per module with its state, flags and revision', async () => {
    const screen = await render(
      <ModuleList
        modules={[
          module(),
          module({ id: 'tiling', state: 'running', enabled: true, paused: true }),
        ]}
      />,
    );
    const cards = screen.getByRole('listitem').elements();

    expect(cards.map((card) => card.querySelector('h3')?.textContent)).toEqual([
      'proxyAudio',
      'tiling',
    ]);
    expect(cards[0]!.querySelector('[data-state]')?.textContent).toBe('Disabled');
    expect(cards[1]!.querySelector('[data-state]')?.textContent).toBe('Running');
    expect(row(cards[0]!, 'Enabled')).toBe('No');
    expect(row(cards[1]!, 'Enabled')).toBe('Yes');
    expect(row(cards[1]!, 'Paused')).toBe('Yes');
    expect(row(cards[0]!, 'Revision')).toBe('3');
  });

  it('renders a revision above Number.MAX_SAFE_INTEGER exactly', async () => {
    const revision = '18446744073709551615';

    expect(String(Number(revision))).not.toBe(revision);
    const screen = await render(<ModuleList modules={[module({ revision })]} />);

    expect(row(screen.getByRole('listitem').element(), 'Revision')).toBe(revision);
  });

  it('labels runtime detail and configuration diagnostic separately', async () => {
    const screen = await render(
      <ModuleList
        modules={[
          module({
            state: 'waitingForPermission',
            detail: 'needs Accessibility',
            diagnostic: 'bad key',
          }),
        ]}
      />,
    );

    await expect.element(screen.getByText('Waiting for permission')).toBeVisible();
    expect(
      screen.getByRole('heading', { name: 'Runtime detail' }).element().nextElementSibling
        ?.textContent,
    ).toBe('needs Accessibility');
    expect(
      screen.getByRole('heading', { name: 'Configuration diagnostic' }).element().nextElementSibling
        ?.textContent,
    ).toBe('bad key');
  });
});
