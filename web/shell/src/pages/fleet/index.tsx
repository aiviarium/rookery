// The fleet: hosts, the provisioners that make them, and what runs on them:
// terminals, desktops, jobs, schedules and roll-outs.
//
// At the top of the sidebar the fleet is one row that drills in. Inside it are
// its places and its hosts, and a host drills into its own places.
import { lazy } from "solid-js";
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { HostDomain } from "./hosts/domain.tsx";
import { HostFilesPage } from "./hosts/files.tsx";
import { HostJobsPage } from "./hosts/jobs.tsx";
import { HostOverview } from "./hosts/overview.tsx";
import { HostPortsPage } from "./hosts/ports.tsx";
import { HostProfilePage } from "./hosts/profile.tsx";
import { HostSidebar } from "./hosts/sidebar.tsx";
import { HostTabPage } from "./hosts/tab.tsx";
import { HostsPage } from "./hosts/list.tsx";
import { JobPage } from "./jobs/job.tsx";
import { JobsPage } from "./jobs/list.tsx";
import { SchedulesPage } from "./jobs/schedules.tsx";
import { RolloutPage } from "./rollouts/rollout.tsx";
import { RolloutsPage } from "./rollouts/list.tsx";
import { FLEET_PLACES, FleetSidebar, useFleetRow } from "./sidebar.tsx";
import "./fleet.css";

// A terminal draws with xterm.js and a desktop with noVNC. Each is a chunk of
// its own, so a person who opens neither never downloads either.
const TerminalsPage = lazy(() => import("./terminals/page.tsx"), { export: "TerminalsPage" });
const DesktopPage = lazy(() => import("./desktops/page.tsx"), { export: "DesktopPage" });
const HostTerminalPage = lazy(() => import("./hosts/terminal.tsx"), { export: "HostTerminalPage" });

export const routes = defineRoutes([
  { path: "/hosts", component: HostsPage },
  {
    // Every place of a host reads the host once, through HostDomain.
    path: "/hosts/:name",
    component: HostDomain,
    children: [
      { path: "/", component: HostOverview },
      { path: "/terminal", component: HostTerminalPage },
      { path: "/files", component: HostFilesPage },
      { path: "/ports", component: HostPortsPage },
      { path: "/desktop", component: DesktopPage },
      { path: "/jobs", component: HostJobsPage },
      { path: "/profile", component: HostProfilePage },
      { path: "/tab/:plugin/:tab", component: HostTabPage },
    ],
  },
  { path: "/terminals", component: TerminalsPage },
  { path: "/jobs", component: JobsPage },
  { path: "/jobs/:job", component: JobPage },
  { path: "/schedules", component: SchedulesPage },
  { path: "/rollouts", component: RolloutsPage },
  { path: "/rollouts/:rollout", component: RolloutPage },
]);

/** HOST_PATH matches an address inside a host, with its name. */
const HOST_PATH = /^\/hosts\/([^/]+)(?:\/|$)/;

export const levels: Area["levels"] = [
  {
    id: "fleet",
    depth: 1,
    match: (path) => (FLEET_PLACES.some((place) => path === place || path.startsWith(`${place}/`)) ? {} : undefined),
    Sidebar: FleetSidebar,
  },
  {
    id: "host",
    depth: 2,
    match: (path) => {
      const found = HOST_PATH.exec(path);
      return found === null ? undefined : { name: decodeURIComponent(found[1] ?? "") };
    },
    Sidebar: HostSidebar,
  },
];

export const top: Area["top"] = [{ order: 10, use: useFleetRow }];
