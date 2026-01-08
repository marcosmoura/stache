import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import './main.css';

import { Renderer } from './renderer/Renderer';

createRoot(document.getElementById('root') as HTMLElement).render(
  <StrictMode>
    <Renderer />
  </StrictMode>,
);
