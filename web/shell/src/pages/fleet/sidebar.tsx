// The fleet in the sidebar: one row at the top that drills in, and the fleet's
// level, with its places and then its hosts, each of which drills into the
// host's own places.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";

import { type LevelProps, TOP } from "../../chrome/levels.ts";
import { LevelSidebar, type SidebarGroup, at } from "../../chrome/sidebar.tsx";
import { createLive } from "../../parts/data.ts";
import { useShell } from "../../shell.tsx";
import { hostIssue } from "./format.ts";
import { HOST_EVENTS } from "./hosts/paths.ts";

/** FLEET_PLACES are the addresses inside the fleet. */
export const FLEET_PLACES = ["/hosts", "/terminals", "/jobs", "/schedules", "/rollouts"];

/** useHosts is every host the person may see, kept up to date. */
function useHosts() {
  const shell = useShell();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  return () => hosts()?.hosts ?? [];
}

/** Trouble is a short warning beside a sidebar row, such as "2 behind". */
function Trouble(props: { children: JSX.Element }): JSX.Element {
  return <span class="shell-trouble">{props.children}</span>;
}

/** useFleetRow is the fleet's row at the top of the sidebar. */
export function useFleetRow(): () => SidebarGroup[] {
  const hosts = useHosts();
  const behind = () => hosts().filter((host) => hostIssue(host)?.tone === "warn").length;
  return () => [
    {
      items: [
        {
          label: "Fleet",
          icon: "server",
          drill: { id: "fleet" },
          meta: behind() > 0 ? <Trouble>{`${behind()} behind`}</Trouble> : undefined,
        },
      ],
    },
  ];
}

/** FleetSidebar is the fleet's level: its places, then its hosts. */
export function FleetSidebar(_props: LevelProps): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const hosts = useHosts();
  const place = (label: string, href: string, icon: SidebarGroup["items"][number]["icon"]) => ({
    label,
    href,
    icon,
    current: at(location.pathname, href) || (href !== "/hosts" && at(location.pathname, href, true)),
  });
  const groups = (): SidebarGroup[] => [
    {
      items: [
        place("Hosts", "/hosts", "server"),
        place("Terminals", "/terminals", "terminal"),
        place("Jobs", "/jobs", "list-checks"),
        place("Schedules", "/schedules", "calendar-clock"),
        place("Roll-outs", "/rollouts", "git-pull-request"),
      ],
    },
    {
      label: "Hosts",
      action: { label: "Add host", icon: "plus", onClick: () => shell.navigate("/hosts?add=1") },
      items: hosts().map((host) => {
        const issue = hostIssue(host);
        return {
          label: host.name,
          icon: "server" as const,
          drill: { id: "host", params: { name: host.name } },
          meta: issue?.tone === "warn" ? <Trouble>{issue.label.toLowerCase()}</Trouble> : undefined,
        };
      }),
    },
  ];
  return <LevelSidebar label="Fleet" title="Fleet" back={{ label: "Rookery", to: { id: TOP } }} groups={groups()} />;
}
