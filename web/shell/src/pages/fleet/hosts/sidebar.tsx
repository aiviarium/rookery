// The host's level: back to the fleet, the host with a switcher to the others,
// and the host's places. A workstation has a desktop; plugins add places for
// the host types they serve, beside the core's.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { HostType } from "@rookery/proto";
import { StatusDot } from "@aiviarium/nocturne";

import type { LevelProps } from "../../../chrome/levels.ts";
import { LevelSidebar, type SidebarEntry, useSiblings } from "../../../chrome/sidebar.tsx";
import { createLive } from "../../../parts/data.ts";
import { useShell } from "../../../shell.tsx";
import { hostIssue, hostTypeName } from "../format.ts";
import { HOST_EVENTS, hostPath } from "./paths.ts";

/** HostSidebar is the host level's sidebar. */
export function HostSidebar(props: LevelProps): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const name = () => props.params.name ?? "";
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  const host = () => hosts()?.hosts.find((candidate) => candidate.name === name());

  // The place the page is at, so the switcher keeps it on another host.
  const place = () => {
    if (!props.inside) return "";
    const rest = location.pathname.slice(hostPath(name()).length).replace(/^\//, "");
    return rest.startsWith("tab/") ? rest : (rest.split("/")[0] ?? "");
  };
  const item = (label: string, at: string, icon: SidebarEntry["icon"]): SidebarEntry => ({
    label,
    icon,
    href: hostPath(name(), at),
    current: props.inside && place() === at,
  });

  const places = (): SidebarEntry[] => {
    const type = host()?.type ?? HostType.SSH;
    const out = [item("Overview", "", "server"), item("Terminal", "terminal", "terminal"), item("Files", "files", "folder"), item("Ports", "ports", "plug")];
    if (type === HostType.WORKSTATION) out.push(item("Desktop", "desktop", "monitor"));
    out.push(item("Jobs", "jobs", "list-checks"), item("Profile", "profile", "sliders-horizontal"));
    const typeName = hostTypeName(type);
    for (const tab of shell.plugins.hostTabs()) {
      if (tab.types.length === 0 || tab.types.includes(typeName)) out.push(item(tab.label, `tab/${tab.plugin}/${tab.id}`, "puzzle"));
    }
    return out;
  };

  const switcher = useSiblings({
    label: "Switch host",
    items: () => (hosts()?.hosts ?? []).map((candidate) => ({ id: candidate.name, label: candidate.name })),
    current: name,
    inside: () => props.inside,
    path: (other) => hostPath(other, place().startsWith("tab/") ? "" : place()),
    level: (other) => ({ id: "host", params: { name: other } }),
  });

  const issue = () => (host() === undefined ? undefined : hostIssue(host()!));
  return (
    <LevelSidebar
      label={`${name()}'s places`}
      back={{ label: "Fleet", to: { id: "fleet" } }}
      title={name()}
      mark={
        <span class="shell-host-mark">
          <StatusDot status={issue()?.tone ?? (host() === undefined ? "idle" : "ok")} />
        </span>
      }
      switcher={switcher()}
      groups={[{ items: places() }]}
    />
  );
}
