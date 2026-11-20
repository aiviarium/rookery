// The infrastructure area: hosts, their terminals and desktops, and the job a
// host's apply starts. Machines, the jobs list, schedules and roll-outs are the
// other half of it.
//
// The shell's page seam joins this area's routes and navigation with the others;
// the area adds no code anywhere else.
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { DesktopPage } from "./desktops/page.tsx";
import { HostPage } from "./hosts/host.tsx";
import { HostsPage } from "./hosts/list.tsx";
import { JobPage } from "./jobs/job.tsx";
import { TerminalsPage } from "./terminals/page.tsx";
import "./infra.css";

export const routes = defineRoutes([
  { path: "/hosts", component: HostsPage },
  { path: "/hosts/:name", component: HostPage },
  { path: "/hosts/:name/desktop", component: DesktopPage },
  { path: "/terminals", component: TerminalsPage },
  { path: "/terminals/:id", component: TerminalsPage },
  { path: "/jobs/:job", component: JobPage },
]);

export const nav: Area["nav"] = [
  { label: "Hosts", href: "/hosts", group: "Infrastructure", icon: "hosts" },
  { label: "Terminals", href: "/terminals", group: "Infrastructure", icon: "jobs" },
];
