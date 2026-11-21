// The top-level places: the shell's own pages and the installed plugins', in
// groups. The first group has no name and holds Home and the inbox.
import type { IconName } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { nav as coreNav } from "../pages/index.ts";

/** NavLink is one link in the sidebar. */
export interface NavLink {
  readonly label: string;
  readonly href: string;
  readonly icon?: IconName;
  /** count is a number beside the label, such as unread notifications. */
  readonly count?: number;
}

/** NavGroup is a named run of links. */
export interface NavGroup {
  readonly title?: string;
  readonly items: readonly NavLink[];
}

/** navigation reads the groups the sidebar shows, for the person signed in. */
export function navigation(): readonly NavGroup[] {
  const shell = useShell();
  const admin = shell.signIn.user()?.admin === true;
  const groups = new Map<string, NavLink[]>();
  const add = (group: string, item: NavLink) => {
    const items = groups.get(group) ?? [];
    if (!items.some((candidate) => candidate.href === item.href)) items.push(item);
    groups.set(group, items);
  };

  for (const entry of coreNav) {
    if (entry.admin && !admin) continue;
    add(entry.group ?? "", {
      label: entry.label,
      href: entry.href,
      icon: entry.icon,
      count: entry.href === "/inbox" ? shell.notifications.unread() : undefined,
    });
  }
  for (const entry of shell.plugins.nav()) add(entry.group, { label: entry.label, href: entry.href, icon: "puzzle" });

  return [...groups.entries()].map(([title, items]) => (title === "" ? { items } : { title, items }));
}
