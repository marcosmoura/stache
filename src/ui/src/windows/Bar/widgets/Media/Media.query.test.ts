import { QueryObserver, type QueryClient } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { flush, mockHost, type MockHost } from '../../../../../tests/ipc';
import { createQueryClient } from '../../../../../tests/queryClient';

import { ARTWORK_GC_MS, artworkUrl, mediaArtworkOptions } from './Media.query';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);

let client: QueryClient;
let host: MockHost;
let stops: Array<() => void>;

beforeEach(() => {
  client = createQueryClient();
  client.mount();
  host = mockHost();
  stops = [];
});

afterEach(() => {
  stops.forEach((stop) => stop());
  client.clear();
  client.unmount();
});

const observe = (id: string | null) => {
  const observer = new QueryObserver(client, mediaArtworkOptions(id));

  stops.push(observer.subscribe(() => undefined));

  return observer;
};

const settled = async (observer: QueryObserver<string | null>) => {
  for (
    let attempt = 0;
    attempt < 50 && observer.getCurrentResult().fetchStatus !== 'idle';
    attempt += 1
  ) {
    await flush();
    await new Promise((done) => setTimeout(done, 1));
  }

  return observer.getCurrentResult();
};

describe('mediaArtworkOptions', () => {
  it('has the cache options of the spec', () => {
    const options = mediaArtworkOptions(A);

    expect(options.queryKey).toEqual(['bar', 'media', 'artwork', A]);
    expect(options.staleTime).toBe(Infinity);
    expect(options.gcTime).toBe(ARTWORK_GC_MS);
    expect(ARTWORK_GC_MS).toBe(60_000);
    expect(options.retry).toBe(false);
    expect(artworkUrl('QUJD')).toBe('data:image/png;base64,QUJD');
  });

  it('does not run without an id', async () => {
    const result = await settled(observe(null));

    expect(result.status).toBe('pending');
    expect(host.callsOf('bar_media_artwork')).toHaveLength(0);
  });

  it('gives the PNG for ready and null for missing', async () => {
    host.answer('bar_media_artwork', (args: unknown) =>
      (args as { id: string }).id === A ? { kind: 'ready', artwork: 'QUJD' } : { kind: 'missing' },
    );

    expect((await settled(observe(A))).data).toBe('QUJD');
    const missing = await settled(observe(B));

    expect(missing.status).toBe('success');
    expect(missing.data).toBeNull();
  });

  it.each([
    ['unavailable', { kind: 'unavailable', reason: 'stopped', detail: null }],
    ['unauthorized', { kind: 'unauthorized' }],
  ])('treats %s as an error without retry', async (_name, outcome) => {
    host.answer('bar_media_artwork', outcome);
    const result = await settled(observe(A));

    expect(result.status).toBe('error');
    expect(result.data).toBeUndefined();
    expect(host.callsOf('bar_media_artwork')).toHaveLength(1);
  });

  it('treats a rejected call as an error without retry', async () => {
    const result = await settled(observe(A));

    expect(result.status).toBe('error');
    expect(host.callsOf('bar_media_artwork')).toHaveLength(1);
  });

  it('fetches once per id and again for a new id', async () => {
    host.answer('bar_media_artwork', { kind: 'ready', artwork: 'QUJD' });

    await settled(observe(A));
    await settled(observe(A));
    await client.refetchQueries({ stale: true });
    await settled(observe(A));
    expect(host.callsOf('bar_media_artwork').map((call) => call.args)).toEqual([{ id: A }]);

    await settled(observe(B));
    expect(host.callsOf('bar_media_artwork').map((call) => call.args)).toEqual([
      { id: A },
      { id: B },
    ]);
  });
});
