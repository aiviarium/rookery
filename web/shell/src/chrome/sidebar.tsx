// The shared sidebar: its header, the places, and the person at the foot.
//
// At the top level the header shows the space the shell is in, with the
// switcher to the others, and the places are the shell's pages and the
// plugins'. Inside a domain, such as a host, an area's own sidebar takes the
// header and the places: back to the list, the domain with a switcher, and the
// domain's places. The person's card stays at the foot either way.
import { For, Show, createMemo } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Dynamic } from "@solidjs/web";
import { useLocation, useMatch } from "@solidjs/router";
import { Count, type MenuEntry, Nav, type NavItemProps, SidebarHeader } from "@aiviarium/nocturne";

import { domains } from "../pages/index.ts";
import { useShell } from "../shell.tsx";
import { AccountMenu } from "./account-menu.tsx";
import { useChrome } from "./context.ts";
import { navigation } from "./navigation.ts";

/** Sidebar is the shell's navigation, drilled into a domain when the path is inside one. */
export function Sidebar(): JSX.Element {
  const matches = domains.map((domain) => ({ domain, match: useMatch(() => domain.pattern) }));
  const inside = createMemo(() => {
    for (const entry of matches) {
      const match = entry.match();
      if (match !== undefined) return { domain: entry.domain, params: match.params as Record<string, string> };
    }
    return undefined;
  });
  return (
    <div class="shell-sidebar" data-part="shell-sidebar">
      <div class="shell-sidebar__body">
        <Show when={inside()} keyed fallback={<TopSidebar />}>
          {(current) => <Dynamic component={current.domain.Sidebar} params={current.params} />}
        </Show>
      </div>
      <div class="shell-sidebar__foot">
        <AccountMenu />
      </div>
    </div>
  );
}

/** TopSidebar is the sidebar outside every domain: the space, then every place. */
function TopSidebar(): JSX.Element {
  const shell = useShell();
  const chrome = useChrome();
  const location = useLocation();

  const current = (href: string): boolean =>
    location.pathname === href || (href !== "/" && location.pathname.startsWith(`${href}/`));

  const spaces = (): MenuEntry[] => [
    { id: "", label: "All spaces", checked: shell.spaces.current() === "" },
    ...shell.spaces.list().map((space) => ({
      id: space.id,
      label: space.name,
      description: space.kind,
      checked: shell.spaces.current() === space.id,
    })),
  ];
  const place = () => shell.spaces.currentSpace()?.name ?? "Rookery";

  const groups = () =>
    navigation().map((group) => ({
      label: group.title,
      items: group.items.map(
        (item): NavItemProps => ({
          label: item.label,
          href: item.href,
          icon: item.icon,
          current: current(item.href),
          meta: item.count !== undefined && item.count > 0 ? <Count value={item.count} /> : undefined,
          onClick: () => chrome.closeDrawer(),
        }),
      ),
    }));

  return (
    <>
      <SidebarHeader
        title={place()}
        mark={<span class="nc-nav-item__mark shell-mark">{place().slice(0, 1).toUpperCase()}</span>}
        switcher={{ label: "Switch space", items: spaces(), onSelect: (id) => shell.spaces.select(id) }}
        onHideSidebar={() => chrome.setSidebarHidden(true)}
      />
      <Nav label="Places" groups={groups()} />
      <For each={shell.plugins.error() === undefined ? [] : [shell.plugins.error()!]}>
        {(error) => <p class="nc-meta shell-sidebar__note">Plugins could not load: {error.message}</p>}
      </For>
    </>
  );
}

/** DomainSidebarProps are a drilled-in sidebar's header and places. */
export interface DomainSidebarProps {
  /** back is the list the domain sits in, such as "Hosts". */
  readonly back: { label: string; href: string };
  /** title is the domain, such as the host's name. */
  readonly title: string;
  /** mark is drawn before the title. */
  readonly mark?: JSX.Element;
  /** switcher opens the other domains of the kind. */
  readonly switcher?: { label: string; items: MenuEntry[]; onSelect: (id: string) => void };
  /** groups are the domain's places. */
  readonly groups: Array<{ label?: string; items: NavItemProps[] }>;
  /** label names the places for a screen reader. */
  readonly label: string;
}

/** DomainSidebar is the sidebar inside a domain, for an area's Sidebar to draw with. */
export function DomainSidebar(props: DomainSidebarProps): JSX.Element {
  const shell = useShell();
  const chrome = useChrome();
  const groups = () =>
    props.groups.map((group) => ({
      label: group.label,
      items: group.items.map((item) => ({
        ...item,
        onClick: (event: MouseEvent & { currentTarget: HTMLAnchorElement; target: Element }) => {
          const own = item.onClick;
          if (typeof own === "function") own(event);
          chrome.closeDrawer();
        },
      })),
    }));
  return (
    <>
      <SidebarHeader
        title={props.title}
        mark={props.mark}
        back={{ to: props.back.label, onClick: () => shell.navigate(props.back.href) }}
        switcher={props.switcher}
        onHideSidebar={() => chrome.setSidebarHidden(true)}
      />
      <Nav label={props.label} groups={groups()} />
    </>
  );
}
