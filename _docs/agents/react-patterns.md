# React Patterns

## Component File Structure

```text
ComponentName/
├── index.ts                  # export { ComponentName } from './ComponentName';
├── ComponentName.tsx         # React component
├── ComponentName.styles.ts   # Linaria CSS
├── ComponentName.types.ts    # TypeScript interfaces (optional)
├── ComponentName.state.ts    # Business logic (optional)
└── ComponentName.test.tsx    # Tests
```

## Event Listening

**Define constant in `types/tauri-events.ts`:**

```typescript
export const MEDIA_PLAYBACK_CHANGED = 'stache://media/playback-changed';
```

**Listen with hook:**

```typescript
useTauriEvent<MediaPayload>(MEDIA_PLAYBACK_CHANGED, (event) => {
  // Handle event
});
```

## useTauriEventQuery Hook

Combines initial fetch with event subscription:

```typescript
const { data, isLoading } = useTauriEventQuery<BatteryInfo>({
  eventName: BATTERY_STATE_CHANGED,
  initialFetch: () => invoke<BatteryInfo>('get_battery_info'),
  transformFn: (payload) => payload, // Optional transform
});
```

## Styling with Linaria

```typescript
// ComponentName.styles.ts
import { css } from '@linaria/core';
import { colors, motion } from '@/design-system';

export const container = css`
  background: ${colors.surface0};
  border-radius: 8px;
  transition: all ${motion.duration} ${motion.easing};
`;

export const containerActive = css`
  background: ${colors.surface1};
`;
```

```tsx
// ComponentName.tsx
import { cx } from '@linaria/core';
import * as styles from './ComponentName.styles';

export function ComponentName({ active }: Props) {
  return <div className={cx(styles.container, active && styles.containerActive)}>{/* ... */}</div>;
}
```

## Cross-Window State

Use `useStoreQuery` for state shared between windows.
