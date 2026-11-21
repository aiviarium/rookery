// One job: its log as it arrives, with what it is about beside it.
//
// The log is followed from the last line this page has, so a dropped connection
// continues where it stopped. A job that is queued or running can be cancelled; a
// job that failed or was cancelled can be put back in the queue, which keeps its
// id and appends to its log.
import { For, Show, createSignal, onSettled, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import type { Job, JobServiceFollowResponse } from "@rookery/proto";
import { Callout, LogViewer, PropertyList, State, type LogEntry, type StreamStatus, type ViewCrumb } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, errorMessage, jobActive, jobState, jobTitle, relativeTime, resourceLabel, resourcePath } from "../format.ts";

/** wait pauses between two attempts to follow a job. */
function wait(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

/** JobPage is the job in the path; another job mounts afresh, with its own log. */
export function JobPage(): JSX.Element {
  const params = useParams<{ job: string }>();
  return <For each={[params.job ?? ""]}>{(id) => <JobView id={id} />}</For>;
}

/** JobViewProps are the job and the path to it. */
export interface JobViewProps {
  readonly id: string;
  /** crumbs are the path to the job from where it was opened; the fleet's jobs when left out. */
  readonly crumbs?: (title: string) => ViewCrumb[];
}

/** JobView is one job's page. A host's place shows it under the host. */
export function JobView(props: JobViewProps): JSX.Element {
  const shell = useShell();
  const id = untrack(() => props.id);
  const [job, setJob] = createSignal<Job>();
  const [entries, setEntries] = createSignal<LogEntry[]>([]);
  const [stream, setStream] = createSignal<StreamStatus>("connecting");
  const [failure, setFailure] = createSignal<string>();
  let from = 0n;
  let controller: AbortController | undefined;

  const read = async (): Promise<void> => {
    try {
      const got = await shell.rookery.jobs.get({ job: id });
      if (got.job !== undefined) setJob(got.job);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const apply = (message: JobServiceFollowResponse): void => {
    const event = message.event;
    if (event.case === "line") {
      const line = event.value;
      from = line.seq + 1n;
      setEntries((previous) => [...previous, { id: line.seq.toString(), text: line.line, at: dateOf(line.at)?.toISOString() }]);
      return;
    }
    if (event.case === "job") {
      setJob(event.value);
      if (!jobActive(event.value)) setStream(event.value.state === "failed" ? "error" : "ended");
    }
  };

  const follow = async (): Promise<void> => {
    controller?.abort();
    const mine = new AbortController();
    controller = mine;
    setStream(from === 0n ? "connecting" : "resuming");
    while (!mine.signal.aborted) {
      try {
        for await (const message of shell.rookery.jobs.follow({ job: id, from }, { signal: mine.signal })) {
          if (mine.signal.aborted) return;
          setStream("live");
          setFailure(undefined);
          apply(message);
        }
        if (mine.signal.aborted) return;
        if (jobActive(untrack(job))) {
          setStream("resuming");
          await wait(1000);
          continue;
        }
        setStream(untrack(job)?.state === "failed" ? "error" : "ended");
        void read();
        return;
      } catch (error) {
        if (mine.signal.aborted) return;
        setFailure(errorMessage(error));
        await wait(1000);
        if (mine.signal.aborted) return;
        setStream("resuming");
      }
    }
  };

  onSettled(() => {
    void read().then(follow);
    return () => controller?.abort();
  });

  const cancel = createAction(async () => {
    const cancelled = await shell.rookery.jobs.cancel({ job: id });
    if (cancelled.job !== undefined) setJob(cancelled.job);
  }, { failed: "The job was not cancelled" });
  const retry = createAction(async () => {
    const retried = await shell.rookery.jobs.retry({ job: id });
    if (retried.job !== undefined) setJob(retried.job);
    void follow();
  }, { failed: "The job was not retried" });

  const retriable = () => job()?.state === "failed" || job()?.state === "cancelled";
  const title = () => (job() === undefined ? `Job ${id.slice(0, 10)}` : jobTitle(job()!));

  return (
    <View
      id="job"
      crumbs={props.crumbs?.(title()) ?? [{ label: "Jobs", href: "/jobs" }, { label: title() }]}
      context={<Show when={job()}>{(shown) => <State status={jobState(shown().state).tone}>{jobState(shown().state).label}</State>}</Show>}
      actions={[
        ...(jobActive(job())
          ? [{ id: "cancel", label: cancel.busy() ? "Cancelling…" : "Cancel job", icon: "close" as const, disabled: cancel.busy(), onClick: () => void cancel.run() }]
          : []),
        ...(retriable()
          ? [{ id: "retry", label: retry.busy() ? "Retrying…" : "Retry", icon: "rotate-ccw" as const, disabled: retry.busy(), onClick: () => void retry.run() }]
          : []),
      ]}
      error={failure()}
      scroll={false}
      aside={<Show when={job()}>{(shown) => <JobDetails job={shown()} />}</Show>}
      asideLabel="Job"
    >
      <Show when={job()?.error}>{(message) => <Callout tone="danger">{message()}</Callout>}</Show>
      <LogViewer
        title="Log"
        label="Job log"
        entries={entries()}
        streamState={stream()}
        showTimestamps
        empty={jobActive(job()) ? "Waiting for the first line…" : "The job wrote nothing."}
      />
    </View>
  );
}

function JobDetails(props: { job: Job }): JSX.Element {
  const resource = () => props.job.resource;
  return (
    <div class="shell-view">
      <PropertyList
        items={[
          {
            label: "About",
            value: (
              <Show when={resource() !== ""} fallback="Nothing">
                <Show when={resourcePath(resource())} fallback={<span class="nc-mono">{resource()}</span>}>
                  {(path) => (
                    <a class="shell-link nc-mono" href={path()}>
                      {resourceLabel(resource())}
                    </a>
                  )}
                </Show>
              </Show>
            ),
          },
          { label: "Kind", value: <span class="nc-mono">{props.job.kind}</span> },
          { label: "Created", value: relativeTime(dateOf(props.job.createdAt)) },
          { label: "Started", value: relativeTime(dateOf(props.job.startedAt)) || "Not yet" },
          { label: "Finished", value: relativeTime(dateOf(props.job.finishedAt)) || "Not yet" },
          { label: "Attempt", value: `${props.job.attempts} of ${props.job.maxAttempts}` },
          { label: "Id", value: <span class="nc-mono">{props.job.id}</span> },
        ]}
      />
    </div>
  );
}
