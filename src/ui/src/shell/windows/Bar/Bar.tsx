import { Suspense, memo, use, type ReactNode } from 'react';
import { ErrorBoundary } from 'react-error-boundary';

import { cx } from '@linaria/core';

import { QueryErrorFallback } from '../../components/QueryErrorFallback';

import { BarDataContext, type BarData } from './Bar.data';
import { useBar, useBarDocument } from './Bar.state';
import * as styles from './Bar.styles';
import { BarDataProvider } from './BarDataProvider';
import { Media } from './Media';
import { useMediaArtwork } from './Media/Media.artwork';
import { Spaces } from './Spaces';
import { Status } from './Status';
import { useBarWeather } from './Weather/Weather.query';
import './Bar.root.css';

/** Production data: the bridge fields plus the weather query (never run for fixture data). */
function BridgeBarData({
  data,
  configRevision,
  weatherEpoch,
  mediaArtworkId,
  children,
}: {
  data: BarData;
  configRevision: string | null;
  weatherEpoch: string;
  mediaArtworkId: string | null;
  children: ReactNode;
}) {
  const weather = useBarWeather(configRevision, weatherEpoch);
  const artwork = useMediaArtwork(mediaArtworkId);
  // No artwork yet (or none) leaves `artwork` unset so the displayed image is retained.
  const media = data.media && artwork ? { ...data.media, artwork } : data.media;

  return <BarDataProvider data={{ ...data, media, weather }}>{children}</BarDataProvider>;
}

const BarContent = memo(function BarContent() {
  const { menuHidden, data, configRevision, weatherEpoch, mediaArtworkId } = useBar();
  const explicit = use(BarDataContext);

  const content = (
    <div className={cx(styles.bar, menuHidden && styles.barHidden)}>
      <Spaces />
      <Media />
      <Status />
    </div>
  );

  // Explicit data (tests, the parity gate) wins; production renders the bridge data.
  return explicit ? (
    content
  ) : (
    <BridgeBarData
      data={data}
      configRevision={configRevision}
      weatherEpoch={weatherEpoch}
      mediaArtworkId={mediaArtworkId}
    >
      {content}
    </BridgeBarData>
  );
});

export function Bar() {
  useBarDocument();

  return (
    <ErrorBoundary fallback={<QueryErrorFallback />}>
      <Suspense fallback={null}>
        <BarContent />
      </Suspense>
    </ErrorBoundary>
  );
}
