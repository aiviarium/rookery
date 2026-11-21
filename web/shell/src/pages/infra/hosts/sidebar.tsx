// The sidebar inside a host: back to the hosts, the host with a switcher to the
// others, and the host's places. A workstation has a desktop; plugins add
// places of their own for the host types they serve.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { HostType } from "@rookery/proto";
import { type NavItemProps, StatusDot } from "@aiviarium/nocturne";

import { DomainSidebar } from "../../../chrome/sidebar.tsx";
import { createLive } from "../../../parts/data.ts";
import { useShell } from "../../../shell.tsx";
import { hostState, hostTypeName } from "../format.ts";
import { HOST_EVENTS, hostPath } from "./paths.ts";

/** HostSidebar is the host domain's sidebar. */
export function HostSidebar(props: { params: Record<string, string> }): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const name = () => props.params.name ?? "";
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  const host = () => hosts()?.hosts.find((candidate) => candidate.name === name());

  // The place the path is at, so the switcher keeps it on another host.
  const place = () => {
    const rest = location.pathname.slice(hostPath(name()).length).replace(/^\//, "");
    return rest.split("/")[0] ?? "";
  };
  const item = (label: string, at: string, icon: NavItemProps["icon"]): NavItemProps => ({
    label,
    icon,
    href: hostPath(name(), at),
    current: place() === at,
  });

  const groups = () => {
    const type = host()?.type ?? HostType.SSH;
    const places: NavItemProps[] = [
      item("Overview", "", "server"),
      item("Terminal", "terminal", "terminal"),
      item("Files", "files", "folder"),
      item("Ports", "ports", "plug"),
    ];
    if (type === HostType.WORKSTATION) places.push(item("Desktop", "desktop", "monitor"));
    places.push(item("Jobs", "jobs", "list-checks"), item("Profile", "profile", "sliders-horizontal"));
    const typeName = hostTypeName(type);
    const tabs = shell.plugins
      .hostTabs()
      .filter((tab) => tab.types.length === 0 || tab.types.includes(typeName))
      .map((tab) => item(tab.label, `tab/${tab.plugin}/${tab.id}`, "puzzle"));
    return tabs.length === 0 ? [{ items: places }] : [{ items: places }, { label: "Plugins", items: tabs }];
  };

  return (
    <DomainSidebar
      label={`${name()}'s places`}
      back={{ label: "Hosts", href: "/hosts" }}
      title={name()}
      mark={
        <span class="shell-host-mark">
          <StatusDot status={host() === undefined ? "idle" : hostState(host()!).tone} />
        </span>
      }
      switcher={{
        label: "Switch host",
        items: (hosts()?.hosts ?? []).map((candidate) => ({
          id: candidate.name,
          label: candidate.name,
          checked: candidate.name === name(),
        })),
        onSelect: (other) => shell.navigate(hostPath(other, place())),
      }}
      groups={groups()}
    />
  );
}
