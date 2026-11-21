// The infrastructure area: hosts, their terminals and desktops, machines and
// provisioners, jobs, schedules and roll-outs.
//
// The shell's page seam joins this area's routes and navigation with the others;
// the area adds no code anywhere else.
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
import { MachinesPage } from "./machines/page.tsx";
import { RolloutPage } from "./rollouts/rollout.tsx";
import { RolloutsPage } from "./rollouts/list.tsx";
import "./infra.css";

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
  { path: "/machines", component: MachinesPage },
  { path: "/jobs", component: JobsPage },
  { path: "/jobs/:job", component: JobPage },
  { path: "/schedules", component: SchedulesPage },
  { path: "/rollouts", component: RolloutsPage },
  { path: "/rollouts/:rollout", component: RolloutPage },
]);

/** The domains the sidebar drills into: inside a host, its places. */
export const domains: Area["domains"] = [{ pattern: "/hosts/:name/*", Sidebar: HostSidebar }];

export const nav: Area["nav"] = [
  { label: "Hosts", href: "/hosts", group: "Infrastructure", icon: "server" },
  { label: "Terminals", href: "/terminals", group: "Infrastructure", icon: "terminal" },
  { label: "Machines", href: "/machines", group: "Infrastructure", icon: "cpu" },
  { label: "Jobs", href: "/jobs", group: "Infrastructure", icon: "list-checks" },
  { label: "Schedules", href: "/schedules", group: "Infrastructure", icon: "calendar-clock" },
  { label: "Roll-outs", href: "/rollouts", group: "Infrastructure", icon: "git-pull-request" },
];

