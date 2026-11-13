import { under } from './shared';
import type { FileCopy, OptionCopy } from './types';

const STRATEGY: Record<string, OptionCopy> = {
  exact: { label: 'Exact name' },
  contains: { label: 'Contains' },
  starts_with: { label: 'Starts with' },
  regex: { label: 'Pattern', hint: 'A regular expression, case sensitive' },
};

const device: FileCopy = {
  '': { label: 'Device', order: ['name', 'strategy', 'depends_on'] },
  name: { label: 'Device name' },
  strategy: { label: 'Match', options: STRATEGY },
  depends_on: {
    label: 'Only when connected',
    hint: 'Used only while this other device is present.',
    order: ['name', 'strategy'],
  },
  'depends_on.name': { label: 'Device name' },
  'depends_on.strategy': { label: 'Match', options: STRATEGY },
};

export const proxyAudio: FileCopy = {
  '': { label: 'Audio', order: ['enabled', 'output', 'input'] },
  enabled: { label: 'Enabled', hint: 'Picks your preferred audio devices as they connect.' },
  input: { label: 'Input devices', hint: 'In order of preference.', noun: 'Device' },
  ...under('input.*', device),
  output: { label: 'Output devices', hint: 'In order of preference.', noun: 'Device' },
  ...under('output.*', device),
};
