// Jobs: everything Rookery runs, newest first, as it changes. A job's kind, its
// state and the resource it is about are what a person filters by; the log, the
// cancel and the retry of one job are on its own page.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Job } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Button, Callout, Card, Empty, Field, IconButton, Input, Page, Select, Spinner } from "../../../ui.tsx";
import { jobState, relativeTime, dateOf } from "../format.ts";
import { StateText, Table, type Column } from "../parts.tsx";

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

  const columns: Column<Job>[] = [
    { id: "id", header: "Job", mono: true, value: job => job.id.slice(0, 10) },
    { id: "kind", header: "Kind", mono: true, value: job => job.kind },
    {
      id: "state",
      header: "State",
      value: job => jobState(job.state).label,
      cell: job => {
        const shown = jobState(job.state);
        return <StateText tone={shown.tone}>{shown.label}</StateText>;
      },
    },
    { id: "resource", header: "Resource", mono: true, value: job => job.resource },
    { id: "attempt", header: "Attempt", num: true, value: job => `${job.attempts}/${job.maxAttempts}` },
    { id: "created", header: "Created", value: job => relativeTime(dateOf(job.createdAt)) },
  ];

  return (
    <Page
      title="Jobs"
      description="Every apply, action, provision and plugin job, with its state and its log."
      actions={
        <>
          <IconButton
            label="Clear the filters"
            glyph="close"
            onClick={() => {
              setKind("");
              setState("");
              void jobs.refresh().catch(() => undefined);
            }}
          />
          <IconButton label="Read the jobs again" glyph="jobs" onClick={() => void jobs.refresh()} />
        </>
      }
    >
      <Show when={jobs.error()}>{error => <Callout tone="danger">{error().message}</Callout>}</Show>
      <Card title="Recent jobs">
        <div class="nc-stack">
          <p class="nc-meta">{jobs()?.jobs.length ?? 0} jobs</p>
          <form
            class="nc-row"
            onSubmit={event => {
              event.preventDefault();
              void jobs.refresh().catch(() => undefined);
            }}
          >
            <Field label="Kind">
              <Input value={kind()} placeholder="host.apply" onInput={setKind} />
            </Field>
            <Field label="State">
              <Select
                value={state()}
                options={STATES.map(value => ({ value, label: value === "" ? "Every state" : jobState(value).label }))}
                onChange={chosen => {
                  setState(chosen);
                  void jobs.refresh().catch(() => undefined);
                }}
              />
            </Field>
            <Button type="submit">Filter</Button>
          </form>
          <Show when={!jobs.pending() || jobs() !== undefined} fallback={<Spinner />}>
            <Table
              label="Jobs"
              rows={jobs()?.jobs ?? []}
              columns={columns}
              rowKey={job => job.id}
              onRowClick={job => shell.navigate(jobPath(job.id))}
              empty="No job matches. Jobs appear as hosts apply, actions run and plugins work."
            />
          </Show>
          <Show when={(jobs()?.jobs.length ?? 0) > 0}>
            <span class="nc-meta">Only the newest 200 jobs are listed. A job's page shows its log and its state.</span>
          </Show>
        </div>
      </Card>
      <Show when={jobs() !== undefined && jobs()!.jobs.length === 0 && !jobs.pending()}>
        <Empty title="No jobs match" description="Clear the filters to see every job the server still keeps." />
      </Show>
    </Page>
  );
}
