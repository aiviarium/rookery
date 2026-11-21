// The top level of the sidebar: Home and the inbox, then what the areas add
// (the fleet, the spaces), then the plugins' places. Nothing here knows the
// areas: each adds a section through the page seam, as a plugin adds its views.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Count, Mark } from "@aiviarium/nocturne";

import { topSections } from "../pages/index.ts";
import { useShell } from "../shell.tsx";
import { LevelSidebar, type SidebarGroup, at } from "./sidebar.tsx";

/** TopSidebar is the sidebar outside every level. */
export function TopSidebar(): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  // Each section reads what it needs once, here, for as long as the top level shows.
  const sections = [...topSections].sort((a, b) => a.order - b.order).map((section) => section.use());

  const main = (): SidebarGroup => ({
    items: [
      { label: "Home", href: "/", icon: "home", current: location.pathname === "/" },
      {
        label: "Inbox",
        href: "/inbox",
        icon: "inbox",
        current: at(location.pathname, "/inbox"),
        meta: shell.notifications.unread() > 0 ? <Count value={shell.notifications.unread()} /> : undefined,
      },
    ],
  });

  // A plugin's places sit under the group its manifest names, or its own.
  const plugins = (): SidebarGroup[] => {
    const groups = new Map<string, SidebarGroup["items"][number][]>();
    for (const entry of shell.plugins.nav()) {
      const items = groups.get(entry.group) ?? [];
      items.push({ label: entry.label, href: entry.href, icon: "puzzle", current: at(location.pathname, entry.href, true) });
      groups.set(entry.group, items);
    }
    return [...groups.entries()].map(([label, items]) => ({ label, items }));
  };

  return (
    <LevelSidebar
      label="Places"
      title="Rookery"
      mark={<Mark name="Rookery" hue="accent" solid decorative />}
      groups={[main(), ...sections.flatMap((groups) => groups()), ...plugins()]}
    />
  );
}
