// One job: what it is about, how far it is, and its log as it arrives.
//
// The log is followed from the last line this page has, so a dropped connection
// continues where it stopped. A job that is queued or running can be cancelled; a
// job that failed or was cancelled can be put back in the queue, which keeps its
// id and appends to its log.
import { Show, createSignal, onCleanup, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import type { Job, JobServiceFollowResponse } from "@rookery/proto";
import {
  Button,
  Callout,
  Card,
  LogViewer,
  PropertyList,
  State,
  type LogEntry,
  type StreamStatus,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { errorMessage, jobActive, jobState, relativeTime, dateOf } from "../format.ts";

/** wait pauses between two attempts to follow a job. */
function wait(milliseconds: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, milliseconds));
}

export function JobPage(): JSX.Element {
  const shell = useShell();
  const params = useParams<{ job: string }>();
  const id = () => params.job ?? "";

  const [job, setJob] = createSignal<Job>();
  const [entries, setEntries] = createSignal<LogEntry[]>([]);
  const [from, setFrom] = createSignal(0n);
  const [stream, setStream] = createSignal<StreamStatus>("connecting");
  const [failure, setFailure] = createSignal<string>();
  const [busy, setBusy] = createSignal<"cancel" | "retry">();

  let controller: AbortController | undefined;

  const read = async (): Promise<void> => {
    try {
      const got = await shell.rookery.jobs.get({ job: id() });
      if (got.job !== undefined) setJob(got.job);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const apply = (message: JobServiceFollowResponse): void => {
    const event = message.event;
    if (event.case === "line") {
      const line = event.value;
      setFrom(line.seq + 1n);
      setEntries(previous => [
        ...previous,
        { id: line.seq.toString(), text: line.line, at: dateOf(line.at)?.toISOString() },
      ]);
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
    setStream(from() === 0n ? "connecting" : "resuming");
    while (!mine.signal.aborted) {
      try {
        const stream = shell.rookery.jobs.follow({ job: id(), from: from() }, { signal: mine.signal });
        for await (const message of stream) {
          if (mine.signal.aborted) return;
          setStream("live");
          apply(message);
        }
        if (mine.signal.aborted) return;
        if (jobActive(job())) {
          setStream("resuming");
          await wait(1000);
          continue;
        }
        setStream(job()?.state === "failed" ? "error" : "ended");
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
    void (async () => {
      await read();
      await follow();
    })();
    onCleanup(() => controller?.abort());
  });

  const cancel = async (): Promise<void> => {
    if (busy() !== undefined) return;
    setBusy("cancel");
    try {
      const cancelled = await shell.rookery.jobs.cancel({ job: id() });
      if (cancelled.job !== undefined) setJob(cancelled.job);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const retry = async (): Promise<void> => {
    if (busy() !== undefined) return;
    setBusy("retry");
    try {
      const retried = await shell.rookery.jobs.retry({ job: id() });
      if (retried.job !== undefined) setJob(retried.job);
      setFailure(undefined);
      await follow();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  return (
    <Page
      title={job()?.kind === undefined || job()?.kind === "" ? `Job ${id().slice(0, 10)}` : job()!.kind}
      description={`Job ${id()}`}
      context={
        <Show when={job()}>
          {shown => <State status={jobState(shown().state).tone}>{jobState(shown().state).label}</State>}
        </Show>
      }
      actions={() => [
        { id: "refresh", label: "Read the job again", icon: "refresh-cw", onClick: () => void read() },
        ...(jobActive(job())
          ? [
              {
                id: "cancel",
                label: busy() === "cancel" ? "Cancelling…" : "Cancel",
                disabled: busy() !== undefined,
                onClick: () => void cancel(),
              },
            ]
          : []),
        ...(job()?.state === "failed" || job()?.state === "cancelled"
          ? [
              {
                id: "retry",
                label: busy() === "retry" ? "Retrying…" : "Retry",
                primary: true,
                disabled: busy() !== undefined,
                onClick: () => void retry(),
              },
            ]
          : []),
      ]}
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show when={job()}>
        {shown => (
          <Card title="The job">
            <PropertyList
              items={[
                {
                  label: "Resource",
                  value: <span class="nc-code">{shown().resource === "" ? "—" : shown().resource}</span>,
                },
                { label: "Created", value: relativeTime(dateOf(shown().createdAt)) },
                { label: "Attempt", value: `${shown().attempts} of ${shown().maxAttempts}` },
                { label: "Finished", value: relativeTime(dateOf(shown().finishedAt)) || "Not yet" },
              ]}
            />
          </Card>
        )}
      </Show>
      <Show when={job()?.error}>
        {message => (
          <Callout
            tone="danger"
            action={
              <Show when={job()?.state === "failed" || job()?.state === "cancelled"}>
                <Button size="sm" onClick={() => void retry()}>
                  Retry
                </Button>
              </Show>
            }
          >
            {message()}
          </Callout>
        )}
      </Show>
      <LogViewer
        title="Job log"
        label="Job log"
        entries={entries()}
        streamState={stream()}
        showTimestamps
        maxHeight="60vh"
        empty={jobActive(job()) ? "Waiting for the first line…" : "The job wrote nothing."}
      />
    </Page>
  );
}
