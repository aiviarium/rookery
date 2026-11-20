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

import { useShell } from "../../../shell.tsx";
import { Button, Callout, Card, IconButton, Page, Spinner } from "../../../ui.tsx";
import { errorMessage, jobActive, jobState, relativeTime, dateOf } from "../format.ts";
import { Log, StateText, type LogEntry } from "../parts.tsx";

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
  const [streamState, setStreamState] = createSignal("Connecting");
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
      if (!jobActive(event.value)) setStreamState(jobState(event.value.state).label);
    }
  };

  const follow = async (): Promise<void> => {
    controller?.abort();
    const mine = new AbortController();
    controller = mine;
    setStreamState(from() === 0n ? "Connecting" : "Resuming");
    while (!mine.signal.aborted) {
      try {
        const stream = shell.rookery.jobs.follow({ job: id(), from: from() }, { signal: mine.signal });
        for await (const message of stream) {
          if (mine.signal.aborted) return;
          setStreamState("Live");
          apply(message);
        }
        if (mine.signal.aborted) return;
        if (jobActive(job())) {
          setStreamState("Resuming");
          await wait(1000);
          continue;
        }
        setStreamState(jobState(job()?.state ?? "").label);
        void read();
        return;
      } catch (error) {
        if (mine.signal.aborted) return;
        setFailure(errorMessage(error));
        await wait(1000);
        if (mine.signal.aborted) return;
        setStreamState("Resuming");
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
      title={job()?.kind === undefined || job()?.kind === "" ? `Job ${id().slice(0, 10)}` : (job()!.kind)}
      description={`Job ${id()}`}
      actions={
        <>
          <Show when={job()}>
            {shown => <StateText tone={jobState(shown().state).tone}>{jobState(shown().state).label}</StateText>}
          </Show>
          <IconButton label="Read the job again" glyph="jobs" onClick={() => void read()} />
          <Show when={jobActive(job())}>
            <Button variant="danger" onClick={() => void cancel()} disabled={busy() !== undefined}>
              {busy() === "cancel" ? "Cancelling…" : "Cancel"}
            </Button>
          </Show>
          <Show when={job()?.state === "failed" || job()?.state === "cancelled"}>
            <Button variant="primary" onClick={() => void retry()} disabled={busy() !== undefined}>
              {busy() === "retry" ? "Retrying…" : "Retry"}
            </Button>
          </Show>
        </>
      }
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show when={job()} fallback={<Spinner />}>
        {shown => (
          <Card title="The job">
            <dl class="infra-properties">
              <dt class="nc-muted">Resource</dt>
              <dd class="nc-code">{shown().resource === "" ? "—" : shown().resource}</dd>
              <dt class="nc-muted">Created</dt>
              <dd>{relativeTime(dateOf(shown().createdAt))}</dd>
              <dt class="nc-muted">Attempt</dt>
              <dd>
                {shown().attempts} of {shown().maxAttempts}
              </dd>
              <dt class="nc-muted">Finished</dt>
              <dd>{relativeTime(dateOf(shown().finishedAt)) || "Not yet"}</dd>
            </dl>
          </Card>
        )}
      </Show>
      <Show when={job()?.error}>
        {message => (
          <Callout tone="danger">
            <div class="nc-stack">
              <span>{message()}</span>
              <Show when={job()?.state === "failed" || job()?.state === "cancelled"}>
                <Button size="sm" onClick={() => void retry()}>
                  Retry
                </Button>
              </Show>
            </div>
          </Callout>
        )}
      </Show>
      <Log
        title="Job log"
        state={streamState()}
        entries={entries()}
        empty={jobActive(job()) ? "Waiting for the first line…" : "The job wrote nothing."}
      />
    </Page>
  );
}
