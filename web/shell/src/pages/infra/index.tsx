// The infrastructure area: hosts, their terminals and desktops, machines and
// provisioners, jobs, schedules and roll-outs.
//
// The shell's page seam joins this area's routes and navigation with the others;
// the area adds no code anywhere else.
import { lazy } from "solid-js";
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { HostPage } from "./hosts/host.tsx";
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

export const routes = defineRoutes([
  { path: "/hosts", component: HostsPage },
  { path: "/hosts/:name", component: HostPage },
  { path: "/hosts/:name/desktop", component: DesktopPage },
  { path: "/terminals", component: TerminalsPage },
  { path: "/terminals/:id", component: TerminalsPage },
  { path: "/machines", component: MachinesPage },
  { path: "/jobs", component: JobsPage },
  { path: "/jobs/:job", component: JobPage },
  { path: "/schedules", component: SchedulesPage },
  { path: "/rollouts", component: RolloutsPage },
  { path: "/rollouts/:rollout", component: RolloutPage },
]);

export const nav: Area["nav"] = [
  { label: "Hosts", href: "/hosts", group: "Infrastructure", icon: "server" },
  { label: "Terminals", href: "/terminals", group: "Infrastructure", icon: "terminal" },
  { label: "Machines", href: "/machines", group: "Infrastructure", icon: "cpu" },
  { label: "Jobs", href: "/jobs", group: "Infrastructure", icon: "list-checks" },
  { label: "Schedules", href: "/schedules", group: "Infrastructure", icon: "calendar-clock" },
  { label: "Roll-outs", href: "/rollouts", group: "Infrastructure", icon: "git-pull-request" },
];
