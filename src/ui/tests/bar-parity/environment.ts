/**
 * Font verification and the parity-run environment record.
 *
 * Maple Mono NF is system-installed and never bundled. A missing font is a recorded environment
 * failure with a clear message; no substitute font is ever picked.
 */
import { commands } from 'vitest/browser';

export interface FontCheck {
  found: boolean;
  message: string;
  record: Awaited<ReturnType<typeof commands.parityFont>>;
  measured?: {
    advanceH: number;
    expectedAdvanceH: number;
    lineHeight: number;
    fallbackHeight: number;
    fallbackWidth: number;
    distinct: boolean;
  };
}

const FAMILY = "'Maple Mono NF'";
const FALLBACK = "'SF Mono', monospace";

/**
 * The font is the installed Maple Mono NF Medium when `document.fonts.ready` resolves, the
 * measured advance of "H" at 13px equals `hmtx(H) * 13 / unitsPerEm` of the file on disk, the
 * rendered advance of a DOM string matches, and the glyph ink differs from the fallback stack.
 */
export async function checkFont(): Promise<FontCheck> {
  const record = await commands.parityFont();

  await document.fonts.ready;

  if (!record.found) {
    return {
      found: false,
      record,
      message: `Maple Mono NF Medium is not installed (looked for ${record.searched.join(', ')}). Install it system-wide; the parity gate never substitutes a font.`,
    };
  }

  const probe = document.createElement('span');

  probe.textContent = 'HHHHHHHHHH';
  probe.style.cssText = `position:absolute;visibility:hidden;white-space:pre;line-height:normal;font:500 13px ${FAMILY}`;
  document.body.append(probe);
  const domAdvance = probe.getBoundingClientRect().width / 10;
  const lineHeight = probe.getBoundingClientRect().height;

  probe.style.font = `500 13px ${FALLBACK}`;
  const fallbackHeight = probe.getBoundingClientRect().height;
  const fallbackWidth = probe.getBoundingClientRect().width / 10;

  probe.remove();

  const expectedAdvanceH = ((record.advanceH ?? 0) * 13) / (record.unitsPerEm ?? 1);
  // The fallback stack must measure differently, otherwise the advance proves nothing.
  const distinct = fallbackHeight !== lineHeight || fallbackWidth !== domAdvance;
  const measured = {
    advanceH: domAdvance,
    expectedAdvanceH,
    lineHeight,
    fallbackHeight,
    fallbackWidth,
    distinct,
  };
  const ok = Math.abs(domAdvance - expectedAdvanceH) < 1e-6 && distinct;

  return {
    found: ok,
    record,
    measured,
    message: ok
      ? 'Maple Mono NF Medium verified'
      : `Maple Mono NF is not what renders at 13px/500: ${JSON.stringify(measured)}. Parity cannot be accepted with a different font.`,
  };
}

export function environmentRecord(font: FontCheck) {
  const { record } = font;
  const webkit = /AppleWebKit\/([\d.]+).*Version\/([\d.]+)/u.exec(navigator.userAgent);

  return {
    font: {
      found: font.found,
      family: record.family ?? null,
      fullName: record.fullName ?? null,
      version: record.version ?? null,
      fileName: record.fileName,
      fileSha256: record.sha256 ?? null,
      unitsPerEm: record.unitsPerEm ?? null,
      advanceH: record.advanceH ?? null,
      measuredAdvanceHAt13px: font.measured?.advanceH ?? null,
      measuredNormalLineHeightAt13px: font.measured?.lineHeight ?? null,
      expectedAdvanceHAt13px: font.measured?.expectedAdvanceH ?? null,
    },
    os: record.os,
    browser: {
      userAgent: navigator.userAgent,
      webKit: webkit?.[1] ?? null,
      safariVersion: webkit?.[2] ?? null,
    },
    deviceScale: window.devicePixelRatio,
    colorScheme: matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light',
    locale: Intl.DateTimeFormat().resolvedOptions().locale,
    frozen: {
      clock: 'Tue Nov 10 14:05:09 (local fields; Date faked)',
      animations: 'finite animations finished or paused at a stated time; scroll-text paused',
      timezoneNote:
        'clock fields are built from local time, so the label does not depend on the zone',
    },
  };
}
