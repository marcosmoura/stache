import type { BarData } from './Bar.data';

export interface BarState {
  menuHidden: boolean | undefined;
  /** The data the providers have published; absent until a reading arrives. */
  data: BarData;
  /** The accepted config revision of a served Bar; `null` while Bar serves nothing. */
  configRevision: string | null;
  /** The published artwork id of the playing media; `null` when it carries none. */
  mediaArtworkId: string | null;
  /** Changes when a late location consent decision asks for a weather refetch now. */
  weatherEpoch: string;
}
