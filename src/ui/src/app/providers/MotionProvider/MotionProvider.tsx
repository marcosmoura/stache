import type { ReactNode } from 'react';

import { LazyMotion, MotionConfig } from 'motion/react';

import { defaultTransition } from '@/theme/motion';

const loadFeatures = () => import('@/app/features').then((module) => module.motionFeatures);

/**
 * Motion for the whole app: `LazyMotion` in `strict` mode (a full `motion.*`
 * component throws; components use `import * as m from 'motion/react-m'`) with
 * the `domMax` features loaded asynchronously, and `MotionConfig` honoring the
 * user's reduced-motion setting with the default transition.
 */
export function MotionProvider({ children }: { children: ReactNode }) {
  return (
    <LazyMotion features={loadFeatures} strict>
      <MotionConfig reducedMotion="user" transition={defaultTransition}>
        {children}
      </MotionConfig>
    </LazyMotion>
  );
}
