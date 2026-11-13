import { getCurrentWindow } from '@tauri-apps/api/window';

/**
 * Closes the window this page runs in (`core:window:allow-close`). Never
 * throws: outside a Tauri webview, or when the host refuses, nothing happens.
 */
export async function closeCurrentWindow(): Promise<void> {
  try {
    await getCurrentWindow().close();
  } catch {
    // No window to close (outside Tauri) or the close was refused.
  }
}

/**
 * Spread on an element to make it the window's drag region
 * (`core:window:allow-start-dragging`). Tauri's injected script starts a drag
 * on a primary press whose target carries the attribute, so its children must
 * not take pointer events and interactive controls must sit outside it.
 */
export const dragRegion = { 'data-tauri-drag-region': true } as const;
