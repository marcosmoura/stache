import { shell } from './Shell.styles';

/** Development/test surface only; the desktop host opens no windows. */
export function Shell() {
  return (
    <main className={shell}>
      <h1>Stache Next</h1>
      <p>Tray-only foundation. No tools are enabled.</p>
    </main>
  );
}
