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
        name="Stache"
        identifier="com.marcosmoura.stache"
        appPath="/Applications/Stache.app"
      />,
    );

    expect(rows(screen.container)).toEqual([
      ['Name', 'Stache'],
      ['Bundle ID', 'com.marcosmoura.stache'],
      ['Path', '/Applications/Stache.app'],
    ]);
    await expect
      .element(screen.getByText('Make sure you turn on Stache, not another copy of the app.'))
      .toBeVisible();
  });

  it('omits the bundle identifier until known and names an unbundled binary', async () => {
    const screen = await render(<IdentityCard name="Stache" identifier={null} appPath={null} />);

    expect(rows(screen.container)).toEqual([
      ['Name', 'Stache'],
      ['Path', 'not an app bundle'],
    ]);
  });
});
