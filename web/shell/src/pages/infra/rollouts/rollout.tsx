// One roll-out: the hosts it works, the batch each is in, and what became of it.
// Progress arrives on RolloutService.Watch; the buttons steer it, and the work of
// each host is a child job that opens from its row.
import { Show, createSignal, onCleanup, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import type { Rollout, RolloutHost } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Button, Callout, Card, Field, Input, Page, Spinner } from "../../../ui.tsx";
import { errorMessage, relativeTime, rolloutActive, rolloutHostState, rolloutState, dateOf } from "../format.ts";
import { Dialog, PropertyList, StateText, Table, type Column } from "../parts.tsx";

export function RolloutPage(): JSX.Element {
  const shell = useShell();
  const params = useParams<{ rollout: string }>();
  const id = () => params.rollout ?? "";

  const [rollout, setRollout] = createSignal<Rollout>();
  const [failure, setFailure] = createSignal<string>();
  const [busy, setBusy] = createSignal<"pause" | "resume" | "cancel">();
  const [pausing, setPausing] = createSignal(false);
  const [why, setWhy] = createSignal("");
  let controller: AbortController | undefined;

  const read = async (): Promise<void> => {
    try {
      const got = await shell.rookery.rollouts.get({ rollout: id() });
      if (got.rollout !== undefined) setRollout(got.rollout);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const watch = async (): Promise<void> => {
    controller = new AbortController();
    try {
      for await (const message of shell.rookery.rollouts.watch({ rollout: id() }, { signal: controller.signal })) {
        if (message.rollout === undefined) continue;
        setRollout(message.rollout);
        if (!rolloutActive(message.rollout)) break;
      }
    } catch (error) {
      if (!controller.signal.aborted) setFailure(errorMessage(error));
    }
  };

  onSettled(() => {
    void read();
    void watch();
    onCleanup(() => controller?.abort());
  });

  const steer = async (what: "pause" | "resume" | "cancel", reason = ""): Promise<void> => {
    if (busy() !== undefined) return;
    setBusy(what);
    setFailure(undefined);
    try {
      const changed =
        what === "pause"
          ? await shell.rookery.rollouts.pause({ rollout: id(), why: reason })
          : what === "resume"
            ? await shell.rookery.rollouts.resume({ rollout: id() })
            : await shell.rookery.rollouts.cancel({ rollout: id() });
      if (changed.rollout !== undefined) setRollout(changed.rollout);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
      setPausing(false);
    }
  };

  const rows = (): RolloutHost[] => {
    const current = rollout();
    if (current === undefined) return [];
    return [...current.hosts, ...current.skipped].sort((a, b) => a.batch - b.batch || a.host.localeCompare(b.host));
  };

  const state = () => {
    const current = rollout();
    return current === undefined ? undefined : rolloutState(current.pausedBecause === "" ? current.state : "paused");
  };

  const columns: Column<RolloutHost>[] = [
    { id: "host", header: "Host", mono: true, value: host => host.host },
    { id: "batch", header: "Batch", num: true, value: host => `${host.batch}` },
    {
      id: "state",
      header: "State",
      value: host => rolloutHostState(host.state).label,
      cell: host => {
        const shown = rolloutHostState(host.state);
        return <StateText tone={shown.tone}>{shown.label}</StateText>;
      },
    },
    { id: "why", header: "Why", value: host => host.error || host.reason },
    { id: "job", header: "Job", mono: true, value: host => host.job.slice(0, 10) },
  ];

  return (
    <Page
      title={`Roll-out ${id().slice(0, 10)}`}
      description={rollout()?.kind === "host.action" ? "One action, on the hosts in batches." : "An apply, on the hosts in batches."}
      actions={
        <>
          <Show when={state()}>{shown => <StateText tone={shown().tone}>{shown().label}</StateText>}</Show>
          <Button onClick={() => void steer("pause")} disabled={rollout()?.state !== "running" || busy() !== undefined}>
            {busy() === "pause" ? "Pausing…" : "Pause"}
          </Button>
          <Button onClick={() => void steer("resume")} disabled={rollout()?.state !== "paused" || busy() !== undefined}>
            {busy() === "resume" ? "Resuming…" : "Resume"}
          </Button>
          <Button
            variant="danger"
            onClick={() => void steer("cancel")}
            disabled={!rolloutActive(rollout()) || busy() !== undefined}
          >
            {busy() === "cancel" ? "Cancelling…" : "Cancel"}
          </Button>
        </>
      }
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show
        when={rollout()?.pausedBecause}
        fallback={
          <Show when={rollout()?.state === "running"}>
            <Callout tone="info">Running. Its hosts change as their jobs end.</Callout>
          </Show>
        }
      >
        {reason => (
          <Callout tone="warning">
            <div class="nc-row" style={{ "justify-content": "space-between" }}>
              <span>Paused: {reason()}</span>
              <Show when={rollout()?.state === "paused"}>
                <Button size="sm" onClick={() => void steer("resume")}>
                  Resume
                </Button>
              </Show>
            </div>
          </Callout>
        )}
      </Show>
      <Show when={rollout()} fallback={<Spinner />}>
        {current => (
          <Card title="Progress">
            <PropertyList
              items={[
                { label: "Succeeded", value: `${current().succeeded}` },
                { label: "Running", value: `${current().running}` },
                { label: "Pending", value: `${current().pending}` },
                { label: "Failed", value: `${current().failed}` },
                { label: "Started", value: relativeTime(dateOf(current().createdAt)) || "Not yet" },
                { label: "Finished", value: relativeTime(dateOf(current().finishedAt)) },
                {
                  label: "Selection",
                  value: `${current().selection?.labels ?? ""}${(current().selection?.hosts.length ?? 0) > 0 ? ` ${current().selection?.hosts.join(", ")}` : ""}${current().selection?.behind === true ? " behind" : ""}`,
                },
              ]}
            />
          </Card>
        )}
      </Show>
      <Card title="Hosts">
        <Table
          label="Hosts of the roll-out"
          rows={rows()}
          columns={columns}
          rowKey={host => host.host}
          onRowClick={host => {
            if (host.job !== "") shell.navigate(`/jobs/${encodeURIComponent(host.job)}`);
          }}
          empty="The plan named none that can take it."
        />
      </Card>
      <Show when={pausing()}>
        <Dialog
          open
          onClose={() => setPausing(false)}
          title="Pause the roll-out?"
          description="Running hosts finish their work, and no new batch starts until it is resumed."
          footer={
            <>
              <Button onClick={() => setPausing(false)} disabled={busy() !== undefined}>
                Cancel
              </Button>
              <Button variant="primary" onClick={() => void steer("pause", why())} disabled={busy() !== undefined}>
                Pause
              </Button>
            </>
          }
        >
          <Field label="Why" hint="The person who started it is told this.">
            <Input value={why()} placeholder="A person is checking the first batch" onInput={setWhy} />
          </Field>
        </Dialog>
      </Show>
    </Page>
  );
}
