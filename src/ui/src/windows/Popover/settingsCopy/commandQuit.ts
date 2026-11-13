import type { FileCopy } from './types';

export const commandQuit: FileCopy = {
  '': {
    label: 'Command-Q',
    order: ['enabled', 'hold_duration', 'dismiss_delay', 'position', 'animation'],
  },
  enabled: { label: 'Enabled', hint: 'Hold Command-Q to quit, so a tap does nothing.' },
  hold_duration: { label: 'Hold duration', unit: 'ms', hint: 'How long to hold Command-Q.' },
  dismiss_delay: {
    label: 'Hide delay',
    unit: 'ms',
    hint: 'How long the notice stays after you let go early.',
  },
  position: {
    label: 'Position',
    options: {
      bottom: { label: 'Bottom' },
      top: { label: 'Top' },
      left: { label: 'Left' },
      right: { label: 'Right' },
      center: { label: 'Center' },
    },
  },
  animation: {
    label: 'Animation',
    options: {
      fade: { label: 'Fade' },
      slide: { label: 'Slide', hint: 'Slides in from its edge' },
    },
  },
};
