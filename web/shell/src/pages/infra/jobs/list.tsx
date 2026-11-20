// Jobs: everything Rookery runs, newest first, as it changes. A job's kind, its
// state and the resource it is about are what a person filters by; the log, the
// cancel and the retry of one job are on its own page.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Job } from "@rookery/proto";
import {
  Card,
  DataTable,
  type DataColumn,
  FilterBar,
  FilterCount,
  Select,
  State,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { jobState, relativeTime, dateOf } from "../format.ts";

const STATES = ["", "queued", "running", "succeeded", "failed", "cancelled", "interrupted"] as const;

/** jobPath is a job's page. */
export function jobPath(id: string): string {
  return `/jobs/${encodeURIComponent(id)}`;
}

export function JobsPage(): JSX.Element {
  const shell = useShell();
  const [kind, setKind] = createSignal("");
  const [state, setState] = createSignal("");
  const jobs = createQuery(() => shell.rookery.jobs.list({ kind: kind().trim(), state: state(), limit: 200 }));

  onSettled(() =>
    shell.connection.on("rookery/jobs.changed", () => {
      void jobs.refresh().catch(() => undefined);
    }),
  );

  const rows = () => jobs()?.jobs ?? [];

  const columns = (): DataColumn<Job>[] => [
    { id: "id", header: "Job", type: "mono", accessor: job => job.id.slice(0, 10) },
    { id: "kind", header: "Kind", type: "mono", accessor: job => job.kind },
    {
      id: "state",
      header: "State",
      accessor: job => jobState(job.state).label,
      cell: job => <State status={jobState(job.state).tone}>{jobState(job.state).label}</State>,
    },
    { id: "resource", header: "Resource", type: "mono", accessor: job => job.resource },
    { id: "attempt", header: "Attempt", type: "num", accessor: job => `${job.attempts}/${job.maxAttempts}` },
    { id: "created", header: "Created", type: "time", accessor: job => relativeTime(dateOf(job.createdAt)) },
  ];

  const clear = () => {
    setKind("");
    setState("");
    void jobs.refresh().catch(() => undefined);
  };

  return (
    <Page
      title="Jobs"
      description="Every apply, action, provision and plugin job, with its state and its log."
      actions={() => [
        { id: "clear", label: "Clear the filters", icon: "close", onClick: clear },
        { id: "refresh", label: "Read the jobs again", icon: "refresh-cw", onClick: () => void jobs.refresh() },
      ]}
    >
      <Card
        title="Jobs"
        count={rows().length}
        padding="none"
        footer={
          <Show when={rows().length > 0}>
            <span class="nc-meta">Only the newest 200 jobs are listed. A job's page shows its log and its state.</span>
          </Show>
        }
      >
        <div class="shell-pad">
          <FilterBar
            label="Job filters"
            search={
              <TextField
                variant="search"
                label="Kind"
                placeholder="host.apply"
                value={kind()}
                onInput={event => setKind(event.currentTarget.value)}
              />
            }
            end={<FilterCount value={rows().length} unit="Jobs" icon="list-checks" />}
          >
            <Select
              aria-label="State"
              value={state()}
              options={STATES.map(value => ({ value, label: value === "" ? "Every state" : jobState(value).label }))}
              onChange={setState}
            />
          </FilterBar>
        </div>
        <DataTable
          bare
          label="Jobs"
          rows={rows()}
          columns={columns()}
          getRowId={job => job.id}
          loading={jobs.pending() && jobs() === undefined}
          onRowClick={job => shell.navigate(jobPath(job.id))}
          empty={{
            title: "No job matches",
            description: "Jobs appear as hosts apply, actions run and plugins work. Clear the filters to see every one.",
          }}
          error={
            jobs.error() === undefined
              ? undefined
              : { message: "The jobs could not be read.", detail: jobs.error()?.message, onRetry: () => void jobs.refresh() }
          }
        />
      </Card>
    </Page>
  );
}
