import type { FileCopy } from './types';

export const notunes: FileCopy = {
  '': { label: 'Music blocker', order: ['enabled', 'target_app'] },
  enabled: { label: 'Enabled', hint: 'Stops Apple Music and iTunes from opening.' },
  target_app: {
    label: 'Open instead',
    hint: 'Player to open instead, by app or bundle ID.',
  },
};
