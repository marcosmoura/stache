import { useQuery } from '@tanstack/react-query';

import { readBarMediaArtwork } from '../../../../sdk/client';

/** Artwork ids are content hashes (not secret) and the cached bytes are small PNGs. */
const ARTWORK_GC_MS = 60 * 1000;

const artworkKey = (id: string) => ['bar', 'media-artwork', id] as const;

/**
 * The base64 PNG for the published artwork id, or `undefined` while it is pending, when
 * there is no id, and for `missing`/`unavailable`/`unauthorized`/failed reads. The key is the
 * id, so a response for an older id can never be returned for a newer one; callers keep the
 * previously displayed image when this is `undefined` (legacy retained artwork).
 */
export function useMediaArtwork(id: string | null): string | undefined {
  const { data } = useQuery<string | null>({
    queryKey: artworkKey(id ?? ''),
    queryFn: id
      ? async () => {
          const outcome = await readBarMediaArtwork(id);

          if (outcome.kind === 'ready') {
            return outcome.artwork;
          }

          if (outcome.kind === 'missing') {
            return null;
          }

          // Unavailable/unauthorized are errors: no artwork, nothing displayed or cleared.
          throw new Error(`media artwork ${outcome.kind}`);
        }
      : () => null,
    enabled: id !== null,
    staleTime: Infinity,
    gcTime: ARTWORK_GC_MS,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });

  return id !== null ? (data ?? undefined) : undefined;
}
