import { describe, expect, it } from 'vitest';

import { renderForm } from '../../../../../../../tests/settingsForm';
import { useWrite } from '../../utils/form';

import { Section } from './Section';

function Harness({ isSet }: { isSet: boolean }) {
  const write = useWrite();

  return (
    <Section
      path={['gaps']}
      label="Gaps"
      hint="Space around windows."
      defaultOpen={false}
      isSet={isSet}
      inTable
      write={write}
    >
      <p>Body</p>
    </Section>
  );
}

describe('Section', () => {
  it('starts as told, toggles its body and marks a default', async () => {
    const { screen } = await renderForm(<Harness isSet={false} />);
    const toggle = screen.getByRole('button', { name: 'Gaps' });

    await expect
      .element(screen.getByRole('region', { name: 'Gaps' }))
      .toHaveAccessibleDescription('Space around windows.');
    await expect.element(screen.getByText('default')).toBeVisible();
    expect(screen.getByText('Body').query()).toBeNull();
    await toggle.click();
    await expect.element(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect.element(screen.getByText('Body')).toBeVisible();
  });

  it('resets a set table', async () => {
    const { screen, edits } = await renderForm(<Harness isSet />, { value: { gaps: {} } });

    await screen.getByRole('button', { name: 'Reset Gaps to default' }).click();
    expect(edits).toEqual([{ op: 'remove', path: ['gaps'] }]);
  });
});
