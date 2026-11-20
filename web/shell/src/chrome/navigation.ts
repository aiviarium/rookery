// The navigation: the shell's own pages and the installed plugins', in groups.
//
// A group's name comes from the entries; the first group has none, and sits at
// the top with Home in it.
import type { IconName } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { nav as coreNav } from "../pages/index.ts";

/** NavItem is one link in the sidebar. */
export interface NavItem {
  readonly label: string;
  readonly href: string;
  readonly icon?: IconName;
}

/** NavGroup is a named run of links. */
export interface NavGroup {
  readonly title?: string;
  readonly items: readonly NavItem[];
}

/** navigation reads the groups the sidebar shows. */
export function navigation(): readonly NavGroup[] {
  const shell = useShell();
  const groups = new Map<string, NavItem[]>();
  const add = (group: string, item: NavItem) => {
    const items = groups.get(group) ?? [];
    if (!items.some((candidate) => candidate.href === item.href)) items.push(item);
    groups.set(group, items);
  };

  for (const entry of coreNav) add(entry.group ?? "", { label: entry.label, href: entry.href, icon: entry.icon });
  for (const entry of shell.plugins.nav()) add(entry.group, { label: entry.label, href: entry.href });

  return [...groups.entries()].map(([title, items]) => (title === "" ? { items } : { title, items }));
}
