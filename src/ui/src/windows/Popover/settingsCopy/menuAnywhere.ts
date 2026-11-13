import type { FileCopy } from './types';

export const menuAnywhere: FileCopy = {
  '': { label: 'Menu Anywhere', order: ['enabled', 'mouse_button', 'modifiers'] },
  enabled: { label: 'Enabled', hint: "Shows the app's menus where you click." },
  modifiers: {
    label: 'Modifier keys',
    hint: 'Keys to hold while clicking, or none for every click.',
    noun: 'Modifier key',
  },
  'modifiers.*': {
    label: 'Modifier key',
    options: {
      control: { label: 'Control' },
      option: { label: 'Option' },
      command: { label: 'Command' },
      shift: { label: 'Shift' },
    },
  },
  mouse_button: {
    label: 'Mouse button',
    options: {
      right_click: { label: 'Right button' },
      middle_click: { label: 'Middle button' },
    },
  },
};
