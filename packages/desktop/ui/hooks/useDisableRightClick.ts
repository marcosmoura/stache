import { useEffect } from 'react';

let devOverride: boolean | null = null;
let isListenerAttached = false;
const handleContextMenu = (event: MouseEvent) => event.preventDefault();

const isDev = () => devOverride ?? import.meta.env.DEV;

export const attachContextMenuListener = () => {
  if (isDev() || isListenerAttached) {
    return () => {};
  }

  isListenerAttached = true;
  document.addEventListener('contextmenu', handleContextMenu);

  return () => {
    if (!isListenerAttached) {
      return;
    }

    isListenerAttached = false;
    document.removeEventListener('contextmenu', handleContextMenu);
  };
};

export const resetDisableRightClickForTesting = () => {
  if (!isListenerAttached) {
    return;
  }

  isListenerAttached = false;
  document.removeEventListener('contextmenu', handleContextMenu);
};

export const useDisableRightClick = () => {
  useEffect(() => attachContextMenuListener(), []);
};

export const isDisableRightClickDevModeForTesting = () => isDev();

export const setDisableRightClickDevModeForTesting = (value: boolean | null) => {
  devOverride = value;
};
