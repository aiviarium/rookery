'use client';
import { Dialog } from '@base-ui/react/dialog';
import type { FullSearchTriggerProps, SearchTriggerProps } from 'fumadocs-ui/layouts/shared/slots/search-trigger';
import type { ThemeSwitchProps } from 'fumadocs-ui/layouts/shared/slots/theme-switch';
import { useSearchContext } from 'fumadocs-ui/contexts/search';
import { useTheme } from 'next-themes';
import { Moon, Search, Sun } from 'lucide-react';
import { useSyncExternalStore } from 'react';

// Header controls in Nocturne's treatments:
// the search field is `.nc-search`, the search button an `.nc-icon-btn`, and
// the theme switch an `.nc-seg`. All are one control height, 28 px.

/** The search field shown on wide screens. */
export function SearchField({ hideIfDisabled, className, ...props }: FullSearchTriggerProps) {
  const { enabled, hotKey, dialogHandle } = useSearchContext();
  if (hideIfDisabled && !enabled) return null;
  return (
    <Dialog.Trigger
      handle={dialogHandle}
      type="button"
      data-search-full=""
      {...props}
      className={`nc-search-field ${className ?? ''}`}
    >
      <Search aria-hidden className="nc-search-field__icon" />
      <span className="nc-search-field__text">Search docs</span>
      <span className="nc-search-field__keys">
        {hotKey.map((key, i) => (
          <kbd key={i} className="nc-kbd">
            {key.display}
          </kbd>
        ))}
      </span>
    </Dialog.Trigger>
  );
}

/** The search button shown on narrow screens. */
export function SearchButton({ hideIfDisabled, className }: SearchTriggerProps) {
  const { enabled, dialogHandle } = useSearchContext();
  if (hideIfDisabled && !enabled) return null;
  return (
    <Dialog.Trigger
      handle={dialogHandle}
      type="button"
      data-search=""
      aria-label="Search docs"
      className={`nc-icon-btn ${className ?? ''}`}
    >
      <Search aria-hidden />
    </Dialog.Trigger>
  );
}

const subscribe = () => () => {};

/** Light and dark as a segmented control. Nothing is selected until the theme is known in the browser. */
export function ThemeToggle({ className }: ThemeSwitchProps) {
  const { resolvedTheme, setTheme } = useTheme();
  const mounted = useSyncExternalStore(subscribe, () => true, () => false);
  const current = mounted ? resolvedTheme : undefined;
  const items = [
    { value: 'light', label: 'Light theme', Icon: Sun },
    { value: 'dark', label: 'Dark theme', Icon: Moon },
  ];
  return (
    <div role="radiogroup" aria-label="Theme" className={`nc-seg ${className ?? ''}`}>
      {items.map(({ value, label, Icon }) => (
        <button
          key={value}
          type="button"
          role="radio"
          aria-checked={current === value}
          aria-label={label}
          title={label}
          className="nc-seg__item"
          onClick={() => setTheme(value)}
        >
          <Icon aria-hidden />
        </button>
      ))}
    </div>
  );
}
