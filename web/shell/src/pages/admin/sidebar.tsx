// Administration's level: who may do what, the server, its secrets and
// connections, all records and documents, then the pages plugins add.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";

import { type LevelProps, TOP } from "../../chrome/levels.ts";
import { LevelSidebar, type SidebarEntry, type SidebarGroup, at } from "../../chrome/sidebar.tsx";
import { useShell } from "../../shell.tsx";

/** adminPluginPath is the address of a page a plugin adds to Administration. */
export function adminPluginPath(plugin: string, page: string): string {
  return `/admin/p/${encodeURIComponent(plugin)}/${encodeURIComponent(page)}`;
}

/** AdminSidebar is Administration's sidebar. */
export function AdminSidebar(_props: LevelProps): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const place = (label: string, href: string, icon: SidebarEntry["icon"]): SidebarEntry => ({
    label,
    href,
    icon,
    current: at(location.pathname, href, true) || (href === "/admin/people" && location.pathname === "/admin"),
  });
  const groups = (): SidebarGroup[] => [
    { items: [place("People", "/admin/people", "users"), place("Groups", "/admin/groups", "users"), place("Access", "/admin/access", "lock")] },
    {
      label: "Server",
      items: [
        place("Plugins", "/admin/plugins", "puzzle"),
        place("Spaces", "/admin/spaces", "layers"),
        place("Keys", "/admin/keys", "key-round"),
        place("Tailnet", "/admin/tailnet", "link"),
        place("Status", "/admin/server", "activity"),
      ],
    },
    { label: "Credentials", items: [place("Secrets", "/admin/secrets", "lock"), place("Connections", "/admin/connections", "link")] },
    { label: "Data", items: [place("Datastore", "/admin/data", "hard-drive"), place("Documents", "/admin/documents", "file")] },
    {
      label: "Plugins",
      items: shell.plugins.adminPages().map((page) => place(page.label, adminPluginPath(page.plugin, page.id), "puzzle")),
    },
  ];
  return <LevelSidebar label="Administration" title="Administration" back={{ label: "Rookery", to: { id: TOP } }} path={[{ label: "Rookery", to: { id: TOP }, show: "hidden" }]} groups={groups()} />;
}
