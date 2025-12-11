/**
 * Tauri Custom Events
 *
 * This file provides type definitions for all custom events emitted from Rust to React.
 * All event names follow the pattern: `module:event-name`
 *
 * This centralized definition improves visibility and type safety for backend events.
 */

// ============================================================================
// Event Name Constants
// ============================================================================

/**
 * Tiling module events
 */
export const TilingEvents = {
  WORKSPACES_CHANGED: 'tiling:workspaces-changed',
  WINDOW_CREATED: 'tiling:window-created',
  WINDOW_DESTROYED: 'tiling:window-destroyed',
  WINDOW_FOCUSED: 'tiling:window-focused',
  WINDOW_MOVED: 'tiling:window-moved',
  WINDOW_RESIZED: 'tiling:window-resized',
  APP_ACTIVATED: 'tiling:app-activated',
  APP_DEACTIVATED: 'tiling:app-deactivated',
  SCREEN_FOCUSED: 'tiling:screen-focused',
} as const;

/**
 * Menubar module events
 */
export const MenubarEvents = {
  VISIBILITY_CHANGED: 'menubar:visibility-changed',
} as const;

/**
 * KeepAwake module events
 */
export const KeepAwakeEvents = {
  STATE_CHANGED: 'keepawake:state-changed',
} as const;

/**
 * Media module events
 */
export const MediaEvents = {
  PLAYBACK_CHANGED: 'media:playback-changed',
} as const;

/**
 * CLI/IPC module events
 */
export const CliEvents = {
  COMMAND_RECEIVED: 'cli:command-received',
} as const;

// ============================================================================
// Event Payload Types
// ============================================================================

/**
 * Payload for window events (created, destroyed, focused, moved)
 */
export interface WindowEventPayload {
  window_id: number;
  app_name: string;
  title: string;
}

/**
 * Payload for window geometry events (resized)
 */
export interface WindowGeometryPayload {
  window_id: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Payload for screen focus events
 */
export interface ScreenFocusedPayload {
  screen: string;
  is_main: boolean;
  previous_screen?: string;
}

/**
 * Payload for keep awake state changes
 */
export interface KeepAwakePayload {
  locked: boolean;
  desired_awake: boolean;
}

/**
 * Payload for CLI command events
 */
export interface CLIEventPayload {
  name: string;
  data?: string;
}
