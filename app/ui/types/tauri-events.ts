/**
 * Tauri Event Definitions
 *
 * All events emitted from the Rust backend follow a consistent naming convention:
 * `barba://<module>/<event-name>`
 *
 * This file defines TypeScript constants that mirror the Rust events module
 * at `app/native/src/events.rs`. Keep these in sync!
 */

/**
 * Menubar-related events
 */
export const MenubarEvents = {
  /** Emitted when the system menu bar visibility changes. Payload: boolean */
  VISIBILITY_CHANGED: 'barba://menubar/visibility-changed',
} as const;

/**
 * Keep-awake (caffeinate) related events
 */
export const KeepAwakeEvents = {
  /** Emitted when keep-awake state changes. Payload: { locked: boolean, desired_awake: boolean } */
  STATE_CHANGED: 'barba://keepawake/state-changed',
} as const;

/**
 * Media playback related events
 */
export const MediaEvents = {
  /** Emitted when media playback state changes. Payload: MediaInfo object */
  PLAYBACK_CHANGED: 'barba://media/playback-changed',
} as const;

/**
 * Spaces/workspace related events
 *
 * These events are triggered by CLI commands (`barba event ...`) and are used
 * by the Spaces component to refresh workspace and window data.
 */
export const SpacesEvents = {
  /** Emitted when window focus changes. Triggered by: `barba event window-focus-changed`. Payload: void */
  WINDOW_FOCUS_CHANGED: 'barba://spaces/window-focus-changed',
  /** Emitted when workspace changes. Triggered by: `barba event workspace-changed <name>`. Payload: string */
  WORKSPACE_CHANGED: 'barba://spaces/workspace-changed',
} as const;

/**
 * Widget-related events
 */
export const WidgetsEvents = {
  /** Emitted to toggle a widget's visibility. Payload: WidgetConfig */
  TOGGLE: 'barba://widgets/toggle',
  /** Emitted when user clicks outside the widgets window. Payload: void */
  CLICK_OUTSIDE: 'barba://widgets/click-outside',
} as const;

/**
 * Cmd+Q hold-to-quit related events
 */
export const CmdQEvents = {
  /** Emitted when user presses Cmd+Q to show hold-to-quit alert. Payload: string (message) */
  ALERT: 'barba://cmd-q/alert',
} as const;

/**
 * Reload app events
 */
export const AppEvents = {
  /** Emitted to signal that the app should reload. Payload: void */
  RELOAD: 'barba://app/reload',
} as const;
