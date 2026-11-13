import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';

import { Tick02Icon, UnfoldMoreIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';

import { styles } from './FileTab.style';

export type SelectOption = { value: string; title: string; description: string | null };

export type SelectProps = {
  /** The control's id (its row label points at it). */
  id: string;
  /** The accessible name. */
  label: string;
  value: string;
  options: readonly SelectOption[];
  disabled?: boolean;
  onSelect: (value: string) => void;
};

type Place = { top?: number; bottom?: number; right: number };

/**
 * A macOS pop-up button (the value plus an up/down chevron) that opens a
 * menu-styled listbox: each option has its title and a description, the
 * selected one a leading check, the focused one the accent highlight. ↑/↓, Home/End move, Enter/Space choose, Esc or Tab close (Esc
 * does not reach the popover), a click outside or a scroll closes.
 */
export function Select({ id, label, value, options, disabled, onSelect }: SelectProps) {
  const listId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLUListElement>(null);
  const [place, setPlace] = useState<Place | null>(null);
  const current = options.find((option) => option.value === value);

  const close = (refocus: boolean) => {
    setPlace(null);

    if (refocus) {
      trigger.current?.focus();
    }
  };

  useEffect(() => {
    if (place === null) {
      return;
    }

    const items = [...(menu.current?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])];

    (items.find((item) => item.getAttribute('aria-selected') === 'true') ?? items[0])?.focus();

    const onPointer = (event: PointerEvent) => {
      const target = event.target as Node;

      if (!menu.current?.contains(target) && !trigger.current?.contains(target)) {
        setPlace(null);
      }
    };
    const onScroll = (event: Event) => {
      if (!menu.current?.contains(event.target as Node)) {
        setPlace(null);
      }
    };

    document.addEventListener('pointerdown', onPointer, true);
    window.addEventListener('scroll', onScroll, true);

    return () => {
      document.removeEventListener('pointerdown', onPointer, true);
      window.removeEventListener('scroll', onScroll, true);
    };
  }, [place]);

  const open = () => {
    const rect = trigger.current?.getBoundingClientRect();

    if (!rect) {
      return;
    }

    const right = Math.max(8, window.innerWidth - rect.right);
    const below = window.innerHeight - rect.bottom;

    setPlace(
      below >= 200 || below >= rect.top
        ? { top: rect.bottom + 4, right }
        : { bottom: window.innerHeight - rect.top + 4, right },
    );
  };

  const choose = (next: string) => {
    close(true);

    if (next !== value) {
      onSelect(next);
    }
  };

  const onMenuKey = (event: KeyboardEvent<HTMLUListElement>) => {
    const items = [...(menu.current?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    const focusAt = (next: number) => items[(next + items.length) % items.length]?.focus();

    switch (event.key) {
      case 'ArrowDown':
        focusAt(index + 1);
        break;
      case 'ArrowUp':
        focusAt(index - 1);
        break;
      case 'Home':
        focusAt(0);
        break;
      case 'End':
        focusAt(items.length - 1);
        break;
      case 'Enter':
      case ' ': {
        const chosen = items[index]?.dataset.value;

        if (chosen !== undefined) {
          choose(chosen);
        }

        break;
      }
      case 'Escape':
        close(true);
        break;
      case 'Tab':
        close(false);

        return;
      default:
        return;
    }

    event.preventDefault();
    event.stopPropagation();
  };

  return (
    <>
      <button
        ref={trigger}
        id={id}
        type="button"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={place !== null}
        aria-controls={place !== null ? listId : undefined}
        disabled={disabled}
        onClick={() => (place === null ? open() : close(false))}
        onKeyDown={(event) => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            open();
          }
        }}
        {...stylex.props(styles.trigger)}
      >
        <span {...stylex.props(styles.triggerText)}>{current?.title ?? value}</span>
        <span {...stylex.props(styles.triggerChevron)}>
          <Icon icon={UnfoldMoreIcon} size={12} />
        </span>
      </button>
      {place !== null ? (
        <ul
          ref={menu}
          id={listId}
          role="listbox"
          aria-label={label}
          onKeyDown={onMenuKey}
          {...stylex.props(
            styles.menu,
            styles.place(
              place.top === undefined ? 'auto' : `${place.top}px`,
              place.bottom === undefined ? 'auto' : `${place.bottom}px`,
              `${place.right}px`,
            ),
          )}
        >
          {options.map((option) => {
            const selected = option.value === value;

            return (
              <li
                key={option.value}
                role="option"
                aria-selected={selected}
                tabIndex={-1}
                data-value={option.value}
                onClick={() => choose(option.value)}
                {...stylex.props(styles.option)}
              >
                <span {...stylex.props(styles.check)}>
                  {selected ? <Icon icon={Tick02Icon} size={12} strokeWidth={2.5} /> : null}
                </span>
                <span {...stylex.props(styles.optionText)}>
                  <span {...stylex.props(styles.optionTitle)}>{option.title}</span>
                  {option.description ? (
                    <span {...stylex.props(styles.optionHelp)}>{option.description}</span>
                  ) : null}
                </span>
              </li>
            );
          })}
        </ul>
      ) : null}
    </>
  );
}
