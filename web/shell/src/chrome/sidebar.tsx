// The sidebar: the space the shell is in, the switch to another one, and every
// page there is.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Icon, type MenuEntry, Nav, SidebarHeader } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { navigation } from "./navigation.ts";

/** Sidebar is the shell's navigation. */
export function Sidebar(): JSX.Element {
  const shell = useShell();
  const location = useLocation();

  const current = (href: string): boolean =>
    location.pathname === href || (href !== "/" && location.pathname.startsWith(`${href}/`));

  const place = () => shell.spaces.currentSpace()?.name ?? "All spaces";

  // Every space is a choice in the header's switcher, with "All spaces" first.
  const spaces = (): MenuEntry[] => [
    { id: "", label: "All spaces" },
    ...shell.spaces.list().map((space) => ({ id: space.id, label: space.name, description: space.kind })),
  ];

  const groups = () =>
    navigation().map((group) => ({
      label: group.title,
      items: group.items.map((item) => ({
        label: item.label,
        href: item.href,
        icon: item.icon,
        current: current(item.href),
      })),
    }));

  return (
    <>
      <SidebarHeader
        title={place()}
        mark={<Icon name="building-2" />}
        switcher={{
          label: "Switch space",
          items: spaces(),
          onSelect: (id) => shell.spaces.select(id),
        }}
      />
      <Nav label="Main" groups={groups()} />
    </>
  );
}
