import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { ObjectBody } from './ObjectBody';

describe('ObjectBody', () => {
  it('orders keys by the copy, drops constants and groups scalars', async () => {
    const tree = node.object({
      name: node.string(),
      widget: node.const('clock'),
      enabled: node.boolean(),
      gaps: node.object({ inner: node.number() }),
    });
    const { screen } = await renderForm(<ObjectBody node={tree} path={[]} at="" depth={0} />, {
      copy: { '': { label: 'File', order: ['enabled', 'name'] } },
    });

    const labels = [...screen.container.querySelectorAll('label, button[aria-expanded]')].map(
      (element) => element.textContent,
    );

    expect(labels).toEqual(['Enabled', 'Name', 'Gaps', 'Inner']);
    expect(screen.container.textContent).not.toContain('Widget');
  });
});
