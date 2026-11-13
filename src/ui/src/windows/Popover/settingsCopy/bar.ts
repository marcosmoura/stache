import { under } from './shared';
import type { FileCopy, OptionCopy } from './types';

const WIDGETS: Record<string, OptionCopy> = {
  spaces: { label: 'Workspaces', hint: 'Your Tiling workspaces and their apps' },
  media: { label: 'Now playing' },
  weather: { label: 'Weather' },
  cpu: { label: 'CPU' },
  battery: { label: 'Battery' },
  keep_awake: { label: 'Keep Awake' },
  wifi: { label: 'Wi-Fi' },
  clock: { label: 'Clock' },
};

const item: FileCopy = {
  '*': {
    label: 'Widget',
    kindLabel: 'Type',
    valueLabel: 'Widget',
    order: ['hour_cycle', 'show_date', 'show_seconds', 'temperature_alert', 'id'],
    options: WIDGETS,
    variants: {
      enum: { label: 'Standard', hint: 'The widget with its usual settings' },
      ...WIDGETS,
    },
  },
  '*.id': { label: 'Name', hint: 'Optional unique name, in letters, digits and dashes.' },
  '*.widget': { label: 'Widget' },
  '*.temperature_alert': {
    label: 'Alert temperature',
    unit: '°C',
    hint: 'Highlights the CPU at this temperature.',
  },
  '*.hour_cycle': {
    label: 'Time format',
    options: {
      h23: { label: '24-hour' },
      h12: { label: '12-hour', hint: 'With AM and PM' },
    },
  },
  '*.show_date': { label: 'Date' },
  '*.show_seconds': { label: 'Seconds' },
};

const ZONE = 'Widget';

export const bar: FileCopy = {
  '': { label: 'Bar', order: ['enabled', 'height', 'padding', 'layout', 'weather'] },
  enabled: { label: 'Enabled', hint: 'Shows the status bar along the top of the screen.' },
  height: { label: 'Height', unit: 'px' },
  padding: { label: 'Padding', unit: 'px' },
  layout: {
    label: 'Layout',
    hint: 'Which items the bar shows, and where.',
    order: ['start', 'center', 'end'],
  },
  'layout.start': { label: 'Left', noun: ZONE },
  ...under('layout.start', item),
  'layout.center': { label: 'Center', noun: ZONE },
  ...under('layout.center', item),
  'layout.end': { label: 'Right', noun: ZONE },
  ...under('layout.end', item),
  weather: {
    label: 'Weather',
    order: ['provider', 'default_location', 'precise_location', 'refresh_interval', 'api_keys'],
  },
  'weather.api_keys': { label: 'API keys file', hint: 'File with your weather API key.' },
  'weather.default_location': {
    label: 'Default location',
    hint: 'A city, address or coordinates, used without Location.',
  },
  'weather.precise_location': {
    label: 'Precise location',
    hint: 'Off uses your approximate location.',
  },
  'weather.provider': {
    label: 'Provider',
    options: {
      auto: { label: 'Automatic', hint: 'Visual Crossing with an API key, else Open-Meteo' },
      visual_crossing: { label: 'Visual Crossing', hint: 'Needs an API key' },
      open_meteo: { label: 'Open-Meteo', hint: 'Free, no API key needed' },
    },
  },
  'weather.refresh_interval': { label: 'Refresh interval', unit: 'minutes' },
};
