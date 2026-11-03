import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './index';
import './main.css';

const root = document.getElementById('root');
if (!root) throw new Error('Missing UI root');

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
