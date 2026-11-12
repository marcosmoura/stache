---
status: done
---

# 05 — Media

## Scope

The Media widget in `windows/Bar/widgets/Media/`: label, artwork by id, retained artwork, player icons and click. Depends on part 02. No Rust change.

## Requirements

- **Data.** `BarView.media` through a `select`. `null` hides the widget.
- **Label.** Title and artist as today (`Title - Artist`, title alone when the artist is empty), prefixed with `Paused: ` while not playing, in a `ScrollingLabel` at most 560px wide (400px in compact mode). A 24px rounded artwork image sits before the icon when one is loaded.
- **Icon.** By the player's bundle identifier, with today's table of known players and their icon colors on Hugeicons free, and the play-circle default for an unknown player.
- **Artwork.** A `queryOptions` factory keyed by `artworkId` calls `bar_media_artwork(id)`: `staleTime: Infinity`, `gcTime` 60 seconds, no retry. `ready` gives a base64 PNG shown as a `data:` image; `missing` is cached as no artwork; `unavailable`, `unauthorized` and a rejected call are query errors. None of these draws a placeholder.
- **Retained artwork.** The displayed image changes only when a new image has loaded successfully, and clears only when an image fails to load. A payload with no `artworkId`, a pending fetch or a failed fetch keeps the previous image. A response for an older id can never be shown for a newer one. When media becomes `null` the widget hides, and the last image is still there for the next track.
- The image is decoded before it is swapped in, so the artwork never flashes empty between tracks.
- **Click.** `bar_action("media")`.
- Register `media` in the production registry.

## Acceptance criteria

- [x] MIG-05-01: Label rules, the paused prefix, scrolling on overflow and the compact width are covered.
- [x] MIG-05-02: Every player in the table maps to an existing icon; an unknown player uses the default.
- [x] MIG-05-03: Artwork is fetched once per id, never refetched for the same id, and every outcome kind is handled without a placeholder.
- [x] MIG-05-04: The retained-artwork rules hold across: track change with a late artwork, a failed fetch, an image decode error, an out-of-order response, and media going to `null` and back.
- [x] MIG-05-05: The click sends `bar_action("media")`.
- [x] MIG-05-06: `docs/features/bar.md` restores the Media UI section as shipped.
- [x] MIG-05-07: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Widget tests for label, icon and visibility states.
- Artwork query tests against the mocked IPC boundary for each outcome and for id changes.
- Retention tests with real image decoding in WebKit, using small valid and invalid PNG fixtures.
- End-to-end `media.spec.ts`: play a track, deliver artwork, pause, change track with artwork arriving later, stop, start again, click.
- Screenshot baselines for playing with artwork, paused and no artwork, in dark and light.

## Notes

- Behavior reference on `main`: `docs/features/bar.md` (Media UI) and `Media.state.ts`, `Media.constants.ts`, `Media.artwork.ts`, `Media.styles.tsx`.
- Outcome: Hugeicons free has no TIDAL icon, so TIDAL shows the vinyl icon until part 06 re-adds simple-icons and restores the brand icon.
