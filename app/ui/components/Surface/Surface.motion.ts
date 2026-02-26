import { motion } from 'motion/react';

import { motionRaw } from '@/design-system';

import type { MotionableComponent } from './Surface.types';

export const initial = { scale: 0.8, opacity: 0.2 };
export const animate = { scale: 1, opacity: 1 };
export const transition = {
  type: 'spring',
  bounce: 0,
  duration: motionRaw.durationSlow,
} as const;

const motionComponentCache = new WeakMap<MotionableComponent, MotionableComponent>();

export function getMotionComponent(component: MotionableComponent): MotionableComponent {
  let cached = motionComponentCache.get(component);

  if (!cached) {
    cached = motion.create(component) as MotionableComponent;
    motionComponentCache.set(component, cached);
  }

  return cached;
}
