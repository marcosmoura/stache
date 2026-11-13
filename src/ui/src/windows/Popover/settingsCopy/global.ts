import type { FileCopy } from './types';

export const global: FileCopy = {
  '': { label: 'General', order: ['ui', 'exec_on_startup', 'logging'] },
  exec_on_startup: {
    label: 'Run at startup',
    hint: 'Commands to run each time Stache starts.',
    noun: 'Command',
    variants: {
      string: { label: 'One command' },
      array: { label: 'Several commands', hint: 'Run in order, stopping at the first failure' },
    },
  },
  logging: { label: 'Logging', order: ['level', 'debug'] },
  'logging.debug': { label: 'Debug details', hint: 'Adds extra detail for troubleshooting.' },
  'logging.level': {
    label: 'Log level',
    hint: 'The least important messages that get recorded.',
    options: {
      debug: { label: 'Debug', hint: 'Everything, for troubleshooting' },
      info: { label: 'Info', hint: 'Normal activity' },
      warn: { label: 'Warnings', hint: 'Problems Stache recovered from' },
      error: { label: 'Errors', hint: 'Failures only' },
    },
  },
  ui: { label: 'Interface' },
  'ui.appearance': {
    label: 'Appearance',
    options: {
      system: { label: 'Automatic', hint: 'Follows the macOS appearance' },
      light: { label: 'Light' },
      dark: { label: 'Dark' },
    },
  },
};
