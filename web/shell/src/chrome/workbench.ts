// The workbench: the shell's pages as tabs in Nocturne's Dock, frames side by
// side and stacked, each tab with a path of its own.
//
// The focused frame's tab is the one the browser's address names: the sidebar,
// a reload and the back button follow it, and a place the sidebar opens opens
// in it. The layout and each tab's path are kept on the device, so a reload
// brings the same tabs back; a tab's page is drawn the first time it shows.
import { type Accessor, createSignal } from "solid-js";
import {
  type DockEdge,
  type DockLayout,
  dockAdd,
  dockFrameOf,
  dockFrames,
  dockLayout,
  dockRemove,
  dockShow,
  dockSplit,
  dockTabs,
} from "@aiviarium/nocturne";

/** WORKBENCH_KEY is where the device keeps the workbench. */
export const WORKBENCH_KEY = "rookery.workbench";

/** HOME is where a new tab opens. */
export const HOME = "/";

/** TabEntry is what a tab holds: its page's path, and the name the page gave it. */
export interface TabEntry {
  readonly path: string;
  readonly title?: string;
}

/** Saved is the workbench as the device keeps it. */
interface Saved {
  readonly layout: DockLayout;
  readonly tabs: Record<string, TabEntry>;
}

/** Navigator moves a drawn tab's router. */
export type Navigator = (path: string, options?: { replace?: boolean }) => void;

/** Workbench is the tabs, their layout, and what moves them. */
export interface Workbench {
  readonly layout: Accessor<DockLayout>;
  setLayout(layout: DockLayout): void;
  /** focused is the tab the focused frame shows. */
  readonly focused: Accessor<string>;
  /** frameOf is the id of the frame that holds a tab. */
  frameOf(id: string): string | undefined;
  tab(id: string): TabEntry | undefined;
  /** setPath records where a tab's router went. */
  setPath(id: string, path: string): void;
  setTitle(id: string, title: string | undefined): void;
  /** go sends a tab to a path: its router when it is drawn, else where it will start. */
  go(id: string, path: string, options?: { replace?: boolean }): void;
  /** open opens a path in a new tab, in a frame or beside it, and focuses it. */
  open(path: string, place?: { frame?: string; edge?: DockEdge }): string;
  close(id: string): void;
  /** closeOthers closes every tab of a tab's frame but it. */
  closeOthers(id: string): void;
  /** register is a drawn tab's router; the returned function forgets it. */
  register(id: string, navigate: Navigator): () => void;
}

/** createWorkbench restores the device's workbench and shows the address in it. */
export function createWorkbench(address: string): Workbench {
  const saved = load();
  // The tabs and the layout are plain values, current the moment they are
  // written; the signals only say that they changed. A tab's page writes its
  // path while its move is a pending transition, and a signal's value written
  // there would stay inside it until the page settles.
  let layoutNow: DockLayout = saved?.layout ?? dockLayout();
  let tabsNow: Record<string, TabEntry> = saved?.tabs ?? {};
  const [layoutChanged, setLayoutChanged] = createSignal(0);
  const [tabsChanged, setTabsChanged] = createSignal(0);
  const layout = () => {
    layoutChanged();
    return layoutNow;
  };
  const tabs = () => {
    tabsChanged();
    return tabsNow;
  };
  const setTabs = (next: Record<string, TabEntry>) => {
    tabsNow = next;
    setTabsChanged((count) => count + 1);
  };
  const setLayoutSignal = (next: DockLayout) => {
    layoutNow = next;
    setLayoutChanged((count) => count + 1);
  };
  const navigators = new Map<string, Navigator>();

  const save = () => write({ layout: layoutNow, tabs: tabsNow });
  const setLayout = (given: DockLayout) => {
    // A layout made from an older one may name a tab that has closed since;
    // it stays closed, rather than coming back as a frame without a page.
    let next = given;
    for (const id of dockTabs(given)) if (tabsNow[id] === undefined) next = dockRemove(next, id);
    // A tab the layout no longer holds is gone.
    const open = new Set(dockTabs(next));
    const kept = Object.fromEntries(Object.entries(tabsNow).filter(([id]) => open.has(id)));
    if (Object.keys(kept).length !== Object.keys(tabsNow).length) setTabs(kept);
    setLayoutSignal(next);
    save();
  };
  const change = (id: string, next: Partial<TabEntry>) => {
    const was = tabsNow[id];
    if (was === undefined) return;
    if (Object.entries(next).every(([key, value]) => was[key as keyof TabEntry] === value)) return;
    setTabs({ ...tabsNow, [id]: { ...was, ...next } });
    save();
  };

  const focused = () => {
    const current = layout();
    return dockFrames(current).find((frame) => frame.id === current.focused)?.active ?? "";
  };

  const open = (path: string, place: { frame?: string; edge?: DockEdge } = {}) => {
    const id = newTabId();
    setTabs({ ...tabsNow, [id]: { path } });
    const frame = place.frame ?? layoutNow.focused;
    let next = dockAdd(layoutNow, id, frame);
    if (place.edge !== undefined && place.edge !== "center") next = dockSplit(next, id, frame, place.edge);
    setLayout(next);
    return id;
  };

  // The tabs the device kept, less any the layout lost, and always one.
  let start = layoutNow;
  for (const id of dockTabs(start)) if (tabsNow[id] === undefined) start = dockRemove(start, id);
  const placed = new Set(dockTabs(start));
  layoutNow = start;
  tabsNow = Object.fromEntries(Object.entries(tabsNow).filter(([id]) => placed.has(id)));
  // The address shows: in a tab already at it, or in a new one.
  const at = Object.entries(tabsNow).find(([, tab]) => tab.path === address)?.[0];
  if (at !== undefined) setLayout(dockShow(layoutNow, at));
  else open(address);

  return {
    layout,
    setLayout,
    focused,
    frameOf: (id) => dockFrameOf(layout(), id)?.id,
    tab: (id) => tabs()[id],
    setPath: (id, path) => change(id, { path }),
    setTitle: (id, title) => change(id, { title }),
    go: (id, path, options) => {
      // The tab is at the path from now on, before its router has drawn it.
      change(id, { path });
      navigators.get(id)?.(path, options);
    },
    open,
    close: (id) => {
      const next = dockRemove(layoutNow, id);
      setLayout(next);
      if (dockTabs(next).length === 0) open(HOME);
    },
    closeOthers: (id) => {
      const frame = dockFrameOf(layoutNow, id);
      if (frame === undefined) return;
      let next = layoutNow;
      for (const other of frame.tabs) if (other !== id) next = dockRemove(next, other);
      setLayout(next);
    },
    register: (id, navigate) => {
      navigators.set(id, navigate);
      return () => {
        if (navigators.get(id) === navigate) navigators.delete(id);
      };
    },
  };
}

/** tabLabel is a tab's name: the one its page gave, or its path's last part. */
export function tabLabel(tab: TabEntry | undefined): string {
  if (tab === undefined) return "";
  if (tab.title !== undefined && tab.title !== "") return tab.title;
  const path = tab.path.split(/[?#]/)[0] ?? "";
  const last = path.split("/").filter(Boolean).at(-1);
  if (last === undefined) return "Home";
  const word = decodeURIComponent(last).replaceAll("-", " ");
  return word.charAt(0).toUpperCase() + word.slice(1);
}

let sequence = 0;
function newTabId(): string {
  sequence += 1;
  return `tab-${Date.now().toString(36)}${sequence.toString(36)}`;
}

function load(): Saved | undefined {
  try {
    const raw = localStorage.getItem(WORKBENCH_KEY);
    if (raw === null) return undefined;
    const saved = JSON.parse(raw) as Saved;
    if (!Array.isArray(saved?.layout?.columns) || typeof saved.tabs !== "object" || saved.tabs === null) return undefined;
    return saved;
  } catch {
    return undefined;
  }
}

function write(saved: Saved): void {
  try {
    localStorage.setItem(WORKBENCH_KEY, JSON.stringify(saved));
  } catch {
    // Without storage the tabs last for this page.
  }
}
