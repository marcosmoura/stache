import {
  AiBrowserIcon,
  BrowserIcon,
  CheckListIcon,
  CodeSimpleIcon,
  CodeSquareIcon,
  CommandLineIcon,
  ComputerTerminal01Icon,
  DashboardCircleIcon,
  FigmaIcon,
  Folder01Icon,
  MessageMultiple01Icon,
  MusicNote03Icon,
  PaintBoardIcon,
  PlayListIcon,
  SmartPhone01Icon,
  Tv01Icon,
  Vynil02Icon,
  WhatsappIcon,
} from '@hugeicons/core-free-icons';
import { SiProtonvpn, SiTidal, SiTransmission } from '@icons-pack/react-simple-icons';
import { describe, expect, test } from 'vitest';

import { appIcons, ease, getAppIcon, springTransition, workspaceIcons } from './Spaces.constants';

describe('Spaces constants', () => {
  test('workspace icons are legacy verbatim', () => {
    expect(workspaceIcons).toEqual({
      terminal: ComputerTerminal01Icon,
      coding: CodeSimpleIcon,
      browser: AiBrowserIcon,
      music: MusicNote03Icon,
      design: FigmaIcon,
      communication: MessageMultiple01Icon,
      guitar: Vynil02Icon,
      misc: DashboardCircleIcon,
      files: Folder01Icon,
      tasks: CheckListIcon,
    });
    expect(workspaceIcons.unknown).toBeUndefined();
  });

  test('keeps every legacy key and adds exactly the seven approved CFBundleName entries', () => {
    expect(Object.keys(appIcons)).toHaveLength(32 + 7);
    expect(getAppIcon('Zed')).toBe(CodeSquareIcon);
    expect(getAppIcon('Aside')).toBe(AiBrowserIcon);
    expect(getAppIcon('Herdr')).toBe(CommandLineIcon);
    expect(getAppIcon('DeviceHub')).toBe(SmartPhone01Icon);
    expect(getAppIcon('AIOStreams')).toBe(PlayListIcon);
    expect(getAppIcon('Nuvio')).toBe(Tv01Icon);
    expect(getAppIcon('FigCraft')).toBe(PaintBoardIcon);
    expect(getAppIcon('Microsoft Edge Dev')).toBe(BrowserIcon);
    expect(getAppIcon('Zed Preview')).toBe(CodeSquareIcon);
    expect(getAppIcon('Proton VPN')).toBe(SiProtonvpn);
    expect(getAppIcon('TIDAL')).toBe(SiTidal);
    expect(getAppIcon('Transmission')).toBe(SiTransmission);
  });

  test('both the U+200E-prefixed and the plain WhatsApp keys map to the WhatsApp icon', () => {
    expect(Object.keys(appIcons)).toContain('\u200EWhatsApp');
    expect(Object.keys(appIcons)).toContain('WhatsApp');
    expect(getAppIcon('\u200EWhatsApp')).toBe(WhatsappIcon);
    expect(getAppIcon('WhatsApp')).toBe(WhatsappIcon);
  });

  test('unknown names fall back to DashboardCircleIcon after trimming whitespace', () => {
    expect(getAppIcon('Nope')).toBe(DashboardCircleIcon);
    expect(getAppIcon('')).toBe(DashboardCircleIcon);
    expect(getAppIcon('  Zed  ')).toBe(CodeSquareIcon);
    // `trim()` does not remove U+200E, so the legacy key is still needed.
    expect('\u200EWhatsApp'.trim()).toBe('\u200EWhatsApp');
  });

  test('motion values match legacy', () => {
    expect(ease).toEqual([0.42, 0, 0.58, 1]);
    expect(springTransition).toEqual({ type: 'spring', bounce: 0, duration: 0.5 });
  });
});
