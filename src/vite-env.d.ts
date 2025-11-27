/// <reference types="vite/client" />

import 'react';

declare module 'react' {
  interface CSSProperties {
    [key: `--${string}`]: string | number;
  }
}

interface ImportMetaEnv {
  readonly API_KEY_VISUAL_CROSSING: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
