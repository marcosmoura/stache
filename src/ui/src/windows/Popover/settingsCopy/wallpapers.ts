import type { FileCopy } from './types';

export const wallpapers: FileCopy = {
  '': {
    label: 'Wallpapers',
    order: ['enabled', 'path', 'list', 'mode', 'interval', 'blur', 'radius'],
  },
  enabled: { label: 'Enabled', hint: 'Changes your desktop picture on a schedule.' },
  path: { label: 'Folder', hint: 'Uses every image in this folder instead of the list.' },
  list: {
    label: 'Wallpapers',
    hint: 'Image file names, used when no folder is set.',
    noun: 'Wallpaper',
  },
  mode: {
    label: 'Order',
    options: {
      random: { label: 'Random' },
      sequential: { label: 'In order' },
    },
  },
  interval: { label: 'Change every', unit: 's', hint: 'Zero changes it only once.' },
  blur: { label: 'Blur', unit: 'px' },
  radius: { label: 'Corner radius', unit: 'px' },
};
