import { useEffect, useState } from 'react';

/**
 * The artwork to display, retained across tracks. `candidate` is the newest
 * fetched image URL (`null`/`undefined` when there is none yet, no id, a
 * pending or failed fetch, or `missing`): only a candidate that decodes
 * replaces the shown image, and only one that fails to decode clears it.
 * Without a candidate the previous image stays. A decode that finishes after
 * the candidate changed (or after unmount) is ignored, so an older image never
 * replaces a newer one. The state lives in the calling component, which stays
 * mounted while its widget is hidden.
 */
export function useRetainedArtwork(candidate: string | null | undefined) {
  const [shown, setShown] = useState<string | null>(null);

  useEffect(() => {
    if (!candidate) {
      return;
    }

    let current = true;
    const image = new Image();

    image.src = candidate;
    image.decode().then(
      () => {
        if (current) {
          setShown(candidate);
        }
      },
      () => {
        if (current) {
          setShown(null);
        }
      },
    );

    return () => {
      current = false;
    };
  }, [candidate]);

  /** Clears the image when the shown element itself fails to load. */
  const fail = () => setShown(null);

  return { artwork: shown, fail };
}
