import { afterEach, describe, expect, test } from 'vitest';
import { page } from 'vitest/browser';

import { PNG_PIXEL, barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { Media } from './Media';
import type { MediaPayload } from './Media.types';

const spotify: MediaPayload = {
  artist: 'Artist',
  title: 'Title',
  bundleIdentifier: 'com.spotify.client',
  playing: true,
};

async function renderMedia(media: MediaPayload | null) {
  return renderWithBarData(<Media />, barData({ media }));
}

describe('Media', () => {
  test('renders nothing when no media is available', async () => {
    const { screen } = await renderMedia(null);

    expect(screen.container.querySelector('[data-testid="media-container"]')).toBeNull();
  });

  test('renders nothing for an empty label', async () => {
    const { screen } = await renderMedia({ ...spotify, title: '', artist: '' });

    expect(screen.container.querySelector('[data-testid="media-container"]')).toBeNull();
  });

  test('renders the playing label as "title - artist" without a prefix', async () => {
    const { screen } = await renderMedia(spotify);
    const container = screen.container.querySelector('[data-testid="media-container"]');

    expect(container?.tagName).toBe('BUTTON');
    await expect.element(screen.getByText('Title - Artist')).toBeVisible();
    expect(container?.querySelectorAll(':scope > span')).toHaveLength(0);
    expect(container?.querySelector('img')).toBeNull();
  });

  test('renders the title alone without an artist', async () => {
    const { screen } = await renderMedia({ ...spotify, artist: '' });

    await expect.element(screen.getByText('Title', { exact: true })).toBeVisible();
  });

  test('renders the Paused prefix while paused', async () => {
    const { screen } = await renderMedia({ ...spotify, playing: false });

    await expect.element(screen.getByText('Paused:')).toBeVisible();
  });

  test('is a fixed overlay centered on the bar', async () => {
    const { screen } = await renderMedia(spotify);
    const element = screen.container.querySelector<HTMLElement>('[data-testid="media-container"]');

    if (!element) {
      throw new Error('media missing');
    }

    const style = getComputedStyle(element);

    expect(style.position).toBe('fixed');
    expect(style.left).toBe(`${window.innerWidth / 2}px`);
    expect(style.transform).toMatch(/^matrix\(1, 0, 0, 1, -[\d.]+, 0\)$/);
    expect(style.paddingLeft).toBe('1px');
    expect(style.display).toBe('grid');
  });

  test('caps the label at 400px on a laptop viewport', async () => {
    const { screen } = await renderMedia(spotify);
    const label = screen.container.querySelector('[data-testid="media-container"] > div');

    if (!label) {
      throw new Error('label missing');
    }

    expect(getComputedStyle(label).maxWidth).toBe('400px');
  });

  test('shows the preloaded artwork at 24x24 with the label as alt text', async () => {
    const { screen } = await renderMedia({ ...spotify, artwork: PNG_PIXEL });

    await expect.poll(() => screen.container.querySelector('img')).not.toBeNull();

    const image = screen.container.querySelector('img');

    expect(image?.alt).toBe('Title - Artist');
    expect(image?.src.startsWith('data:image/png;base64,')).toBe(true);
    expect(getComputedStyle(image as HTMLImageElement).width).toBe('24px');
    expect(getComputedStyle(image as HTMLImageElement).height).toBe('24px');
    expect(getComputedStyle(image as HTMLImageElement).borderRadius).toBe('10px');
  });

  test('keeps the icon only when the artwork fails to decode', async () => {
    const { screen } = await renderMedia({ ...spotify, artwork: 'not-an-image' });

    await expect.element(screen.getByText('Title - Artist')).toBeVisible();
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(screen.container.querySelector('img')).toBeNull();
  });

  test.each([
    ['com.spotify.client', '#a6e3a1', '22'],
    ['com.microsoft.edgemac.Dev', '#f38ba8', '22'],
    ['com.tidal.desktop', '#cdd6f4', '18'],
    ['org.jeffvli.feishin', '#89dceb', '22'],
    ['com.unknown', '#cdd6f4', '22'],
  ])('renders the player icon for %s', async (bundleIdentifier, color, size) => {
    const { screen } = await renderMedia({ ...spotify, bundleIdentifier });
    const svg = screen.container.querySelector('svg');

    expect(svg?.getAttribute('color') ?? svg?.getAttribute('fill')).toBe(color);
    expect(svg?.getAttribute('width')).toBe(size);
  });

  describe('on a wide display', () => {
    afterEach(async () => {
      await page.viewport(414, 896);
    });

    test('uses the 560px label cap above 2036px', async () => {
      await page.viewport(2560, 900);

      const { screen } = await renderMedia(spotify);
      const label = screen.container.querySelector(
        '[data-testid="media-container"] > div',
      ) as HTMLElement;

      expect(getComputedStyle(label).maxWidth).toBe('560px');
    });
  });
});
