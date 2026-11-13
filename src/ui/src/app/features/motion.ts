import { domMax } from 'motion/react';

/**
 * The Motion feature bundle, loaded asynchronously by `MotionProvider` so the
 * initial chunk carries only the `m` components. `domMax` adds layout
 * animations and drag on top of `domAnimation`.
 */
export default domMax;
