import { describe, expect, it } from 'vitest';

import { render } from '../../../../../tests/render';

import { IdentityCard } from './IdentityCard';

const rows = (container: HTMLElement) =>
  Array.from(container.querySelectorAll('dl > div')).map((row) => [
    row.querySelector('dt')?.textContent,
    row.querySelector('dd')?.textContent,
  ]);

describe('IdentityCard', () => {
  it('lists name, bundle identifier and path', async () => {
    const screen = await render(
      <IdentityCard
        name="Stache Next"
        identifier="com.marcosmoura.stache.next"
        appPath="/Applications/Stache Next.app"
      />,
    );

    expect(rows(screen.container)).toEqual([
      ['Name', 'Stache Next'],
      ['Bundle ID', 'com.marcosmoura.stache.next'],
      ['Path', '/Applications/Stache Next.app'],
    ]);
    await expect
      .element(screen.getByText('Make sure you turn on Stache Next, not Stache.'))
      .toBeVisible();
  });

  it('omits the bundle identifier until known and names an unbundled binary', async () => {
    const screen = await render(
      <IdentityCard name="Stache Next" identifier={null} appPath={null} />,
    );

    expect(rows(screen.container)).toEqual([
      ['Name', 'Stache Next'],
      ['Path', 'not an app bundle'],
    ]);
  });
});
