import { motion } from 'motion/react';

import { motionRaw } from '@/design-system';

import type { MotionableComponent } from './Surface.types';

export const initial = { opacity: 0 };
export const animate = { opacity: 1 };
export const transition = {
  ease: motionRaw.easing.split(',').map(Number) as [number, number, number, number],
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
