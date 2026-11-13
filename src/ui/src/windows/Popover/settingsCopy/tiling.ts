import { EASING, under, windowRule } from './shared';
import type { FileCopy, OptionCopy } from './types';

const LAYOUTS: Record<string, OptionCopy> = {
  dwindle: { label: 'Dwindle', hint: 'Each new window splits the last one' },
  split: { label: 'Split', hint: 'Side by side or stacked, to fit the display' },
  split_vertical: { label: 'Stacked', hint: 'Windows one above the other' },
  split_horizontal: { label: 'Side by side', hint: 'Windows next to each other' },
  monocle: { label: 'Monocle', hint: 'Every window fills the screen' },
  master: { label: 'Master and stack', hint: 'One large window, the rest beside it' },
  grid: { label: 'Grid', hint: 'Windows in rows and columns' },
  floating: { label: 'Floating', hint: 'Windows stay where you put them' },
};

/** One border state: off, a solid color, a gradient or a glow. */
const borderStyle: FileCopy = {
  color: { label: 'Color', hint: 'A hex color, such as #89b4fa.' },
  width: { label: 'Width', unit: 'px' },
  animation: { label: 'Animation', order: ['style', 'duration', 'easing'] },
  'animation.duration': { label: 'Duration', unit: 'ms' },
  'animation.easing': { label: 'Easing', options: EASING },
  'animation.style': {
    label: 'Style',
    options: {
      ping_pong: { label: 'Swap colors', hint: 'Colors trade places, focused window only' },
      once: { label: 'Turn once', hint: 'The gradient spins one full turn' },
      loop: { label: 'Keep turning', hint: 'The gradient spins without stopping' },
    },
  },
  gradient: { label: 'Gradient', order: ['from', 'to', 'angle'] },
  'gradient.angle': { label: 'Angle', unit: '°', hint: '0 runs left to right, 90 bottom to top.' },
  'gradient.from': { label: 'Start color' },
  'gradient.to': { label: 'End color' },
  glow: { label: 'Glow color' },
};

const BORDER_VARIANTS: Record<string, OptionCopy> = {
  boolean: { label: 'On or off' },
  'color+width': { label: 'Solid color' },
  'animation+gradient+width': { label: 'Gradient' },
  'glow+width': { label: 'Glow' },
};

const IN_LAYOUT = 'The focused window in this layout.';

const borderState = (key: string, label: string, hint?: string): FileCopy => ({
  [`borders.${key}`]: {
    label,
    hint,
    variants: BORDER_VARIANTS,
    order: ['color', 'gradient', 'glow', 'width', 'animation'],
  },
  ...under(`borders.${key}`, borderStyle),
});

/** Gaps between windows or from the display edges. */
const gap = (label: string, hint: string): FileCopy => ({
  '': {
    label,
    hint,
    unit: 'px',
    valueLabel: 'Size',
    order: ['horizontal', 'vertical', 'top', 'bottom', 'left', 'right'],
    variants: {
      number: { label: 'Same everywhere' },
      'horizontal+vertical': { label: 'Horizontal and vertical' },
      'bottom+left+right+top': { label: 'Each side' },
    },
  },
  horizontal: { label: 'Horizontal', unit: 'px' },
  vertical: { label: 'Vertical', unit: 'px' },
  top: { label: 'Top', unit: 'px' },
  bottom: { label: 'Bottom', unit: 'px' },
  left: { label: 'Left', unit: 'px' },
  right: { label: 'Right', unit: 'px' },
});

const gaps: FileCopy = {
  ...under('inner', gap('Between windows', 'Space between neighbouring windows.')),
  ...under('outer', gap('Screen edges', 'Space between windows and the screen edges.')),
};

const SIZE_VARIANTS: Record<string, OptionCopy> = {
  number: { label: 'Pixels' },
  string: { label: 'Percentage', hint: 'Of the screen, such as 50%' },
};

const DISPLAY = { label: 'Display', hint: 'Main, secondary or a display name.' };

export const tiling: FileCopy = {
  '': {
    label: 'Tiling',
    order: [
      'enabled',
      'default_layout',
      'workspaces',
      'gaps',
      'master',
      'floating',
      'animations',
      'borders',
      'ignore',
    ],
  },
  enabled: { label: 'Enabled', hint: 'Arranges your windows into tiles automatically.' },
  default_layout: {
    label: 'Default layout',
    hint: 'For workspaces without their own layout.',
    options: LAYOUTS,
  },
  workspaces: {
    label: 'Workspaces',
    hint: 'Empty gives each display one workspace.',
    noun: 'Workspace',
  },
  'workspaces.*': {
    label: 'Workspace',
    order: ['name', 'screen', 'layout', 'rules', 'preset_on_open'],
  },
  'workspaces.*.name': { label: 'Name' },
  'workspaces.*.screen': DISPLAY,
  'workspaces.*.layout': {
    label: 'Layout',
    hint: 'Empty uses the default layout.',
    options: LAYOUTS,
  },
  'workspaces.*.preset_on_open': { label: 'Preset for new windows', hint: 'Not used yet.' },
  'workspaces.*.rules': {
    label: 'Rules',
    hint: 'Windows that open in this workspace.',
    noun: 'Rule',
  },
  ...under('workspaces.*.rules.*', windowRule),
  ignore: { label: 'Ignored windows', hint: 'Windows Tiling never arranges.', noun: 'Rule' },
  ...under('ignore.*', windowRule),
  master: { label: 'Master and stack', order: ['position', 'ratio'] },
  'master.position': {
    label: 'Master position',
    options: {
      left: { label: 'Left' },
      right: { label: 'Right' },
      top: { label: 'Top' },
      bottom: { label: 'Bottom' },
      auto: { label: 'Automatic', hint: 'Left on wide displays, top on tall ones' },
    },
  },
  'master.ratio': {
    label: 'Master size',
    unit: '%',
    hint: 'Share of the screen for the main window.',
  },
  gaps: {
    label: 'Gaps',
    order: ['inner', 'outer'],
    variants: {
      'inner+outer': { label: 'All displays' },
      array: { label: 'Per display' },
    },
    noun: 'Display',
  },
  ...under('gaps', gaps),
  ...under('gaps.*', gaps),
  'gaps.*': { label: 'Display', order: ['screen', 'inner', 'outer'] },
  'gaps.*.screen': DISPLAY,
  floating: { label: 'Floating windows', order: ['default_position', 'presets'] },
  'floating.default_position': {
    label: 'New window position',
    options: {
      center: { label: 'Centered' },
      default: { label: 'Where the app puts it' },
    },
  },
  'floating.presets': {
    label: 'Presets',
    hint: 'Named sizes and positions for windows.',
    noun: 'Preset',
  },
  'floating.presets.*': {
    label: 'Preset',
    order: ['name', 'width', 'height', 'x', 'y', 'center'],
  },
  'floating.presets.*.name': { label: 'Name' },
  'floating.presets.*.width': { label: 'Width', unit: 'px', variants: SIZE_VARIANTS },
  'floating.presets.*.height': { label: 'Height', unit: 'px', variants: SIZE_VARIANTS },
  'floating.presets.*.x': { label: 'Left edge', unit: 'px', variants: SIZE_VARIANTS },
  'floating.presets.*.y': { label: 'Top edge', unit: 'px', variants: SIZE_VARIANTS },
  'floating.presets.*.center': { label: 'Centered', hint: 'Ignores the left and top edges.' },
  animations: { label: 'Animations', order: ['enabled', 'duration', 'easing'] },
  'animations.enabled': { label: 'Animations' },
  'animations.duration': { label: 'Duration', unit: 'ms' },
  'animations.easing': { label: 'Easing', options: EASING },
  borders: {
    label: 'Borders',
    order: [
      'enabled',
      'focused',
      'unfocused',
      'floating',
      'transition',
      'ignore',
      'order',
      'hidpi',
      'dwindle',
      'split',
      'split_vertical',
      'split_horizontal',
      'monocle',
      'master',
      'grid',
    ],
  },
  'borders.enabled': { label: 'Borders', hint: 'Draws a colored outline around tiled windows.' },
  'borders.hidpi': { label: 'Retina quality', hint: 'Draws borders at full screen resolution.' },
  'borders.order': {
    label: 'Placement',
    options: {
      below: { label: 'Below the window', hint: 'The window covers its inner edge' },
      above: { label: 'Above the window', hint: 'Entirely outside the window' },
    },
  },
  'borders.transition': {
    label: 'Focus fade',
    hint: 'Fades colors when focus moves.',
    order: ['duration', 'easing'],
  },
  'borders.transition.duration': { label: 'Duration', unit: 'ms' },
  'borders.transition.easing': { label: 'Easing', options: EASING },
  ...borderState('focused', 'Focused window'),
  ...borderState('unfocused', 'Other windows'),
  ...borderState('floating', 'Floating windows'),
  ...borderState('dwindle', 'Dwindle layout', IN_LAYOUT),
  ...borderState('split', 'Split layout', IN_LAYOUT),
  ...borderState('split_vertical', 'Stacked layout', IN_LAYOUT),
  ...borderState('split_horizontal', 'Side by side layout', IN_LAYOUT),
  ...borderState('monocle', 'Monocle layout', IN_LAYOUT),
  ...borderState('master', 'Master and stack layout', IN_LAYOUT),
  ...borderState('grid', 'Grid layout', IN_LAYOUT),
  'borders.ignore': { label: 'Windows without borders', noun: 'Rule' },
  ...under('borders.ignore.*', windowRule),
};
