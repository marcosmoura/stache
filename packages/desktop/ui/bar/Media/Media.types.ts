export interface MediaPayload {
  album?: string | null;
  artist: string;
  artwork?: string | null;
  bundleIdentifier: string;
  mediaType?: string | null;
  playing: boolean;
  title: string;
}

export interface TransformedMediaPayload {
  label: string;
  prefix: string;
  artwork?: string | null;
  bundleIdentifier: string;
}

export type MediaApp = {
  bundleIdentifier: string;
  name: string;
};
