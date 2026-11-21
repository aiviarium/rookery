// Jobs as rows: the kind, what it is about, when it started, and its state
// unless it simply succeeded. The jobs page lists every one; a host lists its
// own.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Job } from "@rookery/proto";
import { EmptyState, ListRow, RowList, Skeleton, State } from "@aiviarium/nocturne";

import { dateOf, jobState, jobTitle, relativeTime, resourceLabel } from "../format.ts";

/** jobPath is a job's page: under its host when given, so the person stays in the host. */
export function jobPath(id: string, host?: string): string {
  const job = `jobs/${encodeURIComponent(id)}`;
  return host === undefined ? `/${job}` : `/hosts/${encodeURIComponent(host)}/${job}`;
}

/** JobRowsProps are the rows and what to say without any. */
export interface JobRowsProps {
  readonly jobs: readonly Job[] | undefined;
  readonly label: string;
  /** resource shows what each job is about; a host's own list leaves it out. */
  readonly resource?: boolean;
  readonly empty: { title: string };
  /** host opens each job under this host instead of under the fleet's jobs. */
  readonly host?: string;
}

/** JobRows is a list of jobs, each a link to its page. */
export function JobRows(props: JobRowsProps): JSX.Element {
  return (
    <Show when={props.jobs !== undefined} fallback={<div class="shell-pad"><Skeleton width="30%" height="14px" /></div>}>
      <Show
        when={(props.jobs ?? []).length > 0}
        fallback={<EmptyState size="sm" icon="list-checks" title={props.empty.title} />}
      >
        <RowList label={props.label}>
          <For each={props.jobs ?? []} keyed={(job) => job.id}>
            {(job) => {
              const state = () => jobState(job().state);
              const when = () => relativeTime(dateOf(job().startedAt ?? job().createdAt));
              return (
                <ListRow
                  data-job={job().id}
                  href={jobPath(job().id, props.host)}
                  title={jobTitle(job())}
                  secondary={
                    props.resource && job().resource !== "" ? <span class="nc-mono">{resourceLabel(job().resource)}</span> : undefined
                  }
                  detail={job().state === "failed" && job().error !== "" ? job().error : undefined}
                  status={job().state === "succeeded" ? undefined : <State status={state().tone}>{state().label}</State>}
                  meta={when()}
                />
              );
            }}
          </For>
        </RowList>
      </Show>
    </Show>
  );
}
