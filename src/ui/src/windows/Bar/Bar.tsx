import { useEffect, useLayoutEffect, useRef, useState, type Ref } from 'react';
import { createPortal } from 'react-dom';

import * as stylex from '@stylexjs/stylex';
import { useQuery } from '@tanstack/react-query';
import { AnimatePresence, useIsPresent, useReducedMotion, type Transition } from 'motion/react';
import * as m from 'motion/react-m';

import type { BarLayoutItem } from '@/ipc';
import { useFeed } from '@/queries';
import { barFeed, barSnapshotOptions, selectBarAway, selectBarLayout } from '@/queries/bar';
import type { WindowProps } from '@/router';
import { durations, easings, transitions } from '@/theme/motion';

import { styles } from './Bar.style';
import './Bar.css';
import { createWidgetNodes, type WidgetNodes } from './utils/nodes';
import { resolveWidget, useWidgetRegistry, WidgetSlot, type WidgetZone } from './widgets';

const ZONES: readonly WidgetZone[] = ['start', 'center', 'end'];

const INSTANT: Transition = { duration: 0 };

/** The slide-away: the Bar's own height down and transparent, over the Bar duration. */
const SLIDE: Transition = { duration: durations.bar, ease: easings.inOut };
const SHOWN = { y: 0, opacity: 1 };
const AWAY = { y: '100%', opacity: 0 };

type Placed = BarLayoutItem & { zone: WidgetZone };

const signature = (placed: readonly Placed[]) =>
  placed.map(({ id, widget, zone }) => `${zone}:${id}:${widget}`).join('|');

/**
 * What the Bar keeps across layout changes: the last placed items, the ids
 * shown before the last change (so a zone move is not faded in) and the
 * removed items whose exit animation is still running (their widgets stay
 * mounted until it completes).
 */
type Hosted = {
  signature: string;
  placed: readonly Placed[];
  previous: ReadonlySet<string>;
  exiting: readonly Placed[];
};

/** Renders one item's widget into its persistent node, from a single keyed list. */
function WidgetHost({ nodes, item }: { nodes: WidgetNodes; item: Placed }) {
  const node = nodes.acquire(item.id);

  useEffect(() => {
    nodes.adopt(item.id, node);

    return () => nodes.release(item.id, node);
  }, [nodes, item.id, node]);

  return createPortal(<WidgetSlot {...item} />, node);
}

type ZoneSlotProps = {
  ref?: Ref<HTMLDivElement>;
  nodes: WidgetNodes;
  item: BarLayoutItem;
  zone: WidgetZone;
  transition: Transition;
  /** The id was already shown elsewhere (a zone move): appear without a fade-in. */
  moved: boolean;
  /** Called when this box's exit animation completes, with the zone it left. */
  onExited: (id: string, zone: WidgetZone) => void;
};

/**
 * The animated box an item occupies in its zone. While present it holds the
 * item's persistent node; an exiting box (an item removed or moved to another
 * zone) never claims it back. A removed item's widget stays mounted in the node
 * inside the exiting box until `onExited`.
 */
function ZoneSlot({ ref, nodes, item, zone, transition, moved, onExited }: ZoneSlotProps) {
  const isPresent = useIsPresent();
  const own = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (isPresent) {
      own.current?.append(nodes.acquire(item.id));
    }
  }, [isPresent, nodes, item.id]);

  return (
    <m.div
      ref={(element: HTMLDivElement | null) => {
        own.current = element;

        if (typeof ref === 'function') {
          ref(element);
        } else if (ref) {
          ref.current = element;
        }
      }}
      {...stylex.props(styles.slot, zone === 'center' && styles.shrinkable)}
      data-item={isPresent ? item.id : undefined}
      data-exiting={isPresent ? undefined : item.id}
      data-widget={item.widget}
      layout="position"
      initial={moved ? false : { opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={transition}
      onAnimationComplete={() => {
        if (!isPresent) {
          onExited(item.id, zone);
        }
      }}
    />
  );
}

/**
 * The Bar window: three zones laid out from `bar.toml` `[layout]`. Start hugs
 * the leading edge, center is centered on the Bar whatever the other zones'
 * widths, end hugs the trailing edge. Each item keeps its widget instance while
 * its `id` survives a layout change, including a move to another zone.
 * While the system menu bar is on screen the Bar slides down by its own height
 * and fades out; the outer element carries the Bar window's root rules.
 */
export function Bar(_props: WindowProps) {
  useFeed(barFeed);

  const { data: frame } = useQuery({ ...barSnapshotOptions(), select: selectBarLayout });
  const { data: away = false } = useQuery({ ...barSnapshotOptions(), select: selectBarAway });
  const registry = useWidgetRegistry();
  const [nodes] = useState(createWidgetNodes);
  const reduced = useReducedMotion();
  const transition = reduced ? INSTANT : transitions.standard;

  const zones = ZONES.map((zone) => ({
    zone,
    items: (frame?.layout[zone] ?? []).filter(
      (item) => resolveWidget(registry, item.widget) !== null,
    ),
  }));
  const placed: Placed[] = zones.flatMap(({ zone, items }) =>
    items.map((item) => ({ ...item, zone })),
  );
  const current = signature(placed);
  const [hosted, setHosted] = useState<Hosted>(() => ({
    signature: current,
    placed,
    previous: new Set(),
    exiting: [],
  }));

  if (hosted.signature !== current) {
    // Adjusting state while rendering: the removed items are known before the
    // commit that would otherwise unmount their widgets.
    const ids = new Set(placed.map((item) => item.id));
    const removed = hosted.placed.filter((item) => !ids.has(item.id));
    const kept = hosted.exiting.filter(
      (item) => !ids.has(item.id) && !removed.some((gone) => gone.id === item.id),
    );

    setHosted({
      signature: current,
      placed,
      previous: new Set([...hosted.placed, ...hosted.exiting].map((item) => item.id)),
      exiting: [...kept, ...removed],
    });
  }

  // Only the exit of the box in the zone the item was removed from ends it:
  // an item that moved zones and was then removed has an older box still
  // fading in its first zone, and that box's exit must not unmount it.
  const onExited = (id: string, zone: WidgetZone) => {
    const done = (item: Placed) => item.id === id && item.zone === zone;

    setHosted((state) =>
      state.exiting.some(done)
        ? { ...state, exiting: state.exiting.filter((item) => !done(item)) }
        : state,
    );
  };
  const live = new Set(placed.map((item) => item.id));
  const exiting = hosted.exiting.filter((item) => !live.has(item.id));

  return (
    <div
      {...stylex.props(styles.window)}
      data-bar-window=""
      onContextMenu={(event) => event.preventDefault()}
    >
      <m.div
        {...stylex.props(styles.root, frame && styles.sized(frame.height))}
        data-surface="bar"
        data-away={away || undefined}
        aria-hidden={away || undefined}
        initial={false}
        animate={away ? AWAY : SHOWN}
        transition={reduced ? INSTANT : SLIDE}
      >
        {zones.map(({ zone, items }) => (
          <div key={zone} {...stylex.props(styles.zone, styles[zone])} data-zone={zone}>
            <AnimatePresence initial={false} mode="popLayout">
              {items.map((item) => (
                <ZoneSlot
                  key={item.id}
                  nodes={nodes}
                  item={item}
                  zone={zone}
                  transition={transition}
                  moved={hosted.previous.has(item.id)}
                  onExited={onExited}
                />
              ))}
            </AnimatePresence>
          </div>
        ))}
        {[...placed, ...exiting].map((item) => (
          <WidgetHost key={item.id} nodes={nodes} item={item} />
        ))}
      </m.div>
    </div>
  );
}
