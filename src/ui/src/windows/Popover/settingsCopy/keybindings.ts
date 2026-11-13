import type { FileCopy } from './types';

export const keybindings: FileCopy = {
  '': { label: 'Shortcuts', order: ['enabled', 'bindings'] },
  enabled: { label: 'Enabled' },
  bindings: {
    label: 'Shortcuts',
    hint: 'Each shortcut runs one or more commands.',
    noun: 'Shortcut',
  },
  'bindings.*': {
    label: 'Commands',
    kindLabel: 'Runs',
    valueLabel: 'Command',
    noun: 'Command',
    variants: {
      string: { label: 'One command' },
      array: { label: 'Several commands', hint: 'Run in order, stopping at the first failure' },
    },
  },
};
