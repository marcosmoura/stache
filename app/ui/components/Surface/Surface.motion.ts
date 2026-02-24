import { motion } from 'motion/react';

import { motionRaw } from '@/design-system';

import type { MotionableComponent } from './Surface.types';

export const initial = { y: '-100%', opacity: 0 };
export const animate = { y: '0%', opacity: 1 };
export const transition = {
  duration: motionRaw.durationSlow,
  ease: motionRaw.easing.split(',').map(Number) as [number, number, number, number],
};

const motionComponentCache = new WeakMap<MotionableComponent, MotionableComponent>();

export function getMotionComponent(component: MotionableComponent): MotionableComponent {
  let cached = motionComponentCache.get(component);

  if (!cached) {
    cached = motion.create(component) as MotionableComponent;
    motionComponentCache.set(component, cached);
  }

  return cached;
}
