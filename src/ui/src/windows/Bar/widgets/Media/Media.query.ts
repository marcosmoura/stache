import { queryOptions, skipToken } from '@tanstack/react-query';

import { commands } from '@/ipc';

/** How long an unobserved artwork stays cached. */
export const ARTWORK_GC_MS = 60_000;

/**
 * The base64 PNG of an artwork id (a non-secret content hash), or `null` for
 * `missing`. `unavailable`, `unauthorized` and a rejected call are errors.
 * Keyed by the id: fetched once per id, never refetched, never retried, and a
 * response for an older id can never answer a newer one. Without an id the
 * query does not run.
 */
export function mediaArtworkOptions(artworkId: string | null) {
  return queryOptions<string | null>({
    queryKey: ['bar', 'media', 'artwork', artworkId],
    queryFn:
      artworkId === null
        ? skipToken
        : async () => {
            const outcome = await commands.barMediaArtwork(artworkId);

            switch (outcome.kind) {
              case 'ready':
                return outcome.artwork;
              case 'missing':
                return null;
              default:
                throw new Error(`Media artwork ${outcome.kind}`);
            }
          },
    staleTime: Infinity,
    gcTime: ARTWORK_GC_MS,
    retry: false,
  });
}

/** The `data:` URL of a base64 PNG. */
export const artworkUrl = (base64: string) => `data:image/png;base64,${base64}`;
