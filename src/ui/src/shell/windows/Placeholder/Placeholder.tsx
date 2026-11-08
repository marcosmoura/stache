import { placeholder } from './Placeholder.styles';

/** Development/test surface only; the desktop host opens no windows. */
export function Placeholder() {
  return (
    <main className={placeholder}>
      <h1>Stache Next</h1>
      <p>Tray-only foundation. No tools are enabled.</p>
    </main>
  );
}
