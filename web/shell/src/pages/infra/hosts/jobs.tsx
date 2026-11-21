// The jobs about one host: applies, actions and its roll-out steps, newest
// first. Each row opens the job's log.
import type { JSX } from "@solidjs/web";

import { createLive } from "../../../parts/data.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { JobRows } from "../jobs/rows.tsx";
import { hostCrumbs, useHost } from "./domain.tsx";
import { hostResource } from "./paths.ts";

/** JOB_EVENTS are the events that change a list of jobs. */
const JOB_EVENTS = ["rookery/jobs."] as const;

/** HostJobsPage is the jobs place of a host. */
export function HostJobsPage(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const jobs = createLive(
    () => shell.rookery.jobs.list({ resource: hostResource(domain.name()), limit: 100 }),
    { key: domain.name, events: JOB_EVENTS },
  );
  return (
    <View
      id="host-jobs"
      crumbs={hostCrumbs(domain.name(), "Jobs")}
      actions={[{ id: "refresh", label: "List the jobs again", icon: "refresh-cw", onClick: () => void jobs.refresh().catch(() => undefined) }]}
      error={jobs.error()?.message}
    >
      <JobRows
        jobs={jobs()?.jobs}
        label={`Jobs on ${domain.name()}`}
        empty={{ title: "No jobs yet", description: "Applying the host or running one of its actions starts a job." }}
      />
    </View>
  );
}
