// The chrome's own state, for the parts inside it: whether the sidebar is
// hidden, the level it was drilled to by hand, and closing the phone drawer
// after a choice.
import { type Accessor, createContext, useContext } from "solid-js";

import type { LevelRef } from "./levels.ts";

/** Chrome is what a view and the sidebar ask of the frame around them. */
export interface Chrome {
  /** sidebarHidden is true while the person hid the sidebar on a desktop. */
  readonly sidebarHidden: Accessor<boolean>;
  /** setSidebarHidden hides the sidebar, or brings it back. */
  setSidebarHidden(hidden: boolean): void;
  /** closeDrawer closes the sidebar's drawer on a phone, after a link in it was followed. */
  closeDrawer(): void;
  /** drilled is the level the person moved the sidebar to by hand, until the next page. */
  readonly drilled: Accessor<LevelRef | undefined>;
  /** drill moves the sidebar to a level and leaves the page as it is. */
  drill(level: LevelRef): void;
}

/** ChromeContext is the chrome, for the views and the sidebar. */
export const ChromeContext = createContext<Chrome | null>(null);

/** useChrome reads the chrome, or a still one outside it (in a test that renders one page). */
export function useChrome(): Chrome {
  return (
    useContext(ChromeContext) ?? {
      sidebarHidden: () => false,
      setSidebarHidden: () => undefined,
      closeDrawer: () => undefined,
      drilled: () => undefined,
      drill: () => undefined,
    }
  );
}
