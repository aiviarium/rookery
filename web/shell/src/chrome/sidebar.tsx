// The shared sidebar: the level it shows, and the person at the foot.
//
// The level is the deepest one that holds the page, unless the person drilled
// the sidebar by hand since the page opened. Each level draws its own header
// and places with LevelSidebar.
import { For, createContext, createMemo, untrack, useContext } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Dynamic } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { cbor } from "@rookery/client";
import { type IconName, type MenuEntry, Nav, type NavItemProps, type PathShow, SidebarHeader } from "@aiviarium/nocturne";

import { levels } from "../pages/index.ts";
import { createLive } from "../parts/data.ts";
import { useShell } from "../shell.tsx";
import { AccountMenu } from "./account-menu.tsx";
import { useChrome } from "./context.ts";
import { type LevelRef, TOP, resolveLevel, sameLevel } from "./levels.ts";
import { TopSidebar } from "./top.tsx";

/** SIDEBAR_PATH is the person's setting for how much of the way down the sidebar's head shows. */
export const SIDEBAR_PATH = "rookery/sidebar_path";

/** PathLength is that setting: the levels each level chose, or all of them. */
export type PathLength = "short" | "full";

const PathLengthContext = createContext<() => PathLength>(() => "short");

/** Sidebar is the shell's navigation at the level the page or the person chose. */
export function Sidebar(): JSX.Element {
  const shell = useShell();
  const chrome = useChrome();
  const location = useLocation();
  const atPage = createMemo(() => resolveLevel(levels, location.pathname));
  const shown = createMemo(() => chrome.drilled() ?? atPage());
  // A level is drawn afresh when it changes, so its reads are its own.
  const key = () => {
    const level = shown();
    return `${level.id}:${JSON.stringify(level.params ?? {})}`;
  };
  // Read again as the level changes, so a change on the settings page shows once the person moves on.
  const setting = createLive(() => shell.rookery.settings.get({ key: SIDEBAR_PATH }).catch(() => undefined), { key });
  const length = (): PathLength => {
    const raw = setting()?.setting?.valueCbor;
    if (raw === undefined || raw.length === 0) return "short";
    try {
      return cbor.decode<string>(raw) === "full" ? "full" : "short";
    } catch {
      return "short";
    }
  };
  return (
    <PathLengthContext value={length}>
    <div class="shell-sidebar" data-part="shell-sidebar">
      <div class="shell-sidebar__body">
        <For each={[key()]}>
          {() => {
            const level = untrack(shown);
            const found = levels.find((candidate) => candidate.id === level.id);
            const inside = () => sameLevel(atPage(), level);
            if (level.id === TOP || found === undefined) return <TopSidebar />;
            return <Dynamic component={found.Sidebar} params={level.params ?? {}} inside={inside()} />;
          }}
        </For>
      </div>
      <div class="shell-sidebar__foot">
        <AccountMenu />
      </div>
    </div>
    </PathLengthContext>
  );
}

/** SidebarEntry is one row of a level: a place, or a row that drills into a level below. */
export interface SidebarEntry {
  readonly label: string;
  /** href is the place the row opens. */
  readonly href?: string;
  /** drill is the level the row opens in the sidebar, leaving the page as it is. */
  readonly drill?: LevelRef;
  readonly icon?: IconName;
  readonly leading?: JSX.Element;
  /** meta is what is out of the ordinary about the place, such as a count of unread items. */
  readonly meta?: JSX.Element;
  readonly current?: boolean;
}

/** SidebarGroup is a run of rows under an optional heading, with one optional action beside it. */
export interface SidebarGroup {
  readonly label?: string;
  readonly action?: { label: string; icon: IconName; onClick: () => void };
  readonly items: readonly SidebarEntry[];
}

/** LevelSidebarProps are a level's header and places. */
export interface LevelSidebarProps {
  /** back is the level above, named for the button. Absent at the top. */
  readonly back?: { label: string; to: LevelRef };
  /**
   * path is the levels above, from the top, shown before the title as
   * `Fleet / build-01`. Each says how it shows by default: in the row, in the
   * `…` menu, or left out, as the top is, since back already names it. A
   * person whose setting asks for the full path sees every level in the row.
   */
  readonly path?: ReadonlyArray<{ label: string; to: LevelRef; show?: PathShow }>;
  /** title is the level, such as the host's name. */
  readonly title: string;
  readonly mark?: JSX.Element;
  /** switcher opens the level's siblings, such as the other hosts. */
  readonly switcher?: { label: string; items: MenuEntry[]; onSelect: (id: string) => void };
  readonly groups: readonly SidebarGroup[];
  /** label names the places for a screen reader. */
  readonly label: string;
}

/** LevelSidebar draws a level: its header with back and its switcher, then its places. */
export function LevelSidebar(props: LevelSidebarProps): JSX.Element {
  const chrome = useChrome();
  const length = useContext(PathLengthContext);
  const groups = () =>
    props.groups
      .filter((group) => group.items.length > 0 || group.action !== undefined)
      .map((group) => ({
        label: group.label,
        action: group.action,
        items: group.items.map((entry): NavItemProps => {
          const drill = entry.drill;
          return {
            label: entry.label,
            icon: entry.icon,
            leading: entry.leading,
            meta: entry.meta,
            current: entry.current,
            href: drill === undefined ? entry.href : undefined,
            drill: drill !== undefined,
            onClick: () => {
              if (drill !== undefined) chrome.drill(drill);
              else chrome.closeDrawer();
            },
          };
        }),
      }));
  return (
    <>
      <SidebarHeader
        title={props.title}
        mark={props.mark}
        back={props.back === undefined ? undefined : { to: props.back.label, onClick: () => chrome.drill(props.back!.to) }}
        path={(props.path ?? []).map((level) => ({
          label: level.label,
          show: length() === "full" ? "inline" : (level.show ?? "inline"),
          onClick: () => chrome.drill(level.to),
        }))}
        switcher={props.switcher}
      />
      <Nav label={props.label} groups={groups()} />
    </>
  );
}

/**
 * siblings is a level's switcher: picking a sibling keeps the page when the
 * page is in the level, as forge's files become build-01's, and otherwise
 * moves the sidebar alone.
 */
export function useSiblings(options: {
  label: string;
  items: () => ReadonlyArray<{ id: string; label: string; description?: string }>;
  current: () => string;
  inside: () => boolean;
  /** path is a sibling's address for the place the page is at. */
  path: (id: string) => string;
  /** level is a sibling's level, for when the page is elsewhere. */
  level: (id: string) => LevelRef;
}): () => LevelSidebarProps["switcher"] {
  const shell = useShell();
  const chrome = useChrome();
  return () => {
    const items = options.items();
    // A place of a kind always has its switcher, even while it is the only one.
    if (items.length === 0) return undefined;
    return {
      label: options.label,
      items: items.map((item) => ({ id: item.id, label: item.label, description: item.description, checked: item.id === options.current() })),
      onSelect: (id) => {
        if (id === options.current()) return;
        if (options.inside()) shell.navigate(options.path(id));
        else chrome.drill(options.level(id));
      },
    };
  };
}

/** at is true when the address is the place, or under it when `deep`. */
export function at(path: string, href: string, deep = false): boolean {
  const bare = href.split("?")[0] ?? href;
  return path === bare || (deep && path.startsWith(`${bare}/`));
}
