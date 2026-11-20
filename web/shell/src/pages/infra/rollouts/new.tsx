// "New roll-out": choose what runs, which hosts it runs on, and how it is split
// into batches. A plan first shows the hosts it would work and the ones it would
// skip, with the reason, so a selection that names nothing is caught before it
// starts.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import type { RolloutHost } from "@rookery/proto";
import {
  Button,
  Callout,
  DataTable,
  type DataColumn,
  Dialog,
  EmptyState,
  SectionHeading,
  Select,
  State,
  Switch,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { errorMessage, rolloutHostState } from "../format.ts";

export function NewRollout(props: { onClose: () => void; onStarted: (id: string) => void }): JSX.Element {
  const shell = useShell();
  const [kind, setKind] = createSignal<"host.apply" | "host.action">("host.apply");
  const [action, setAction] = createSignal("");
  const [hosts, setHosts] = createSignal("");
  const [labels, setLabels] = createSignal("");
  const [behind, setBehind] = createSignal(false);
  const [size, setSize] = createSignal("1");
  const [percent, setPercent] = createSignal("");
  const [failures, setFailures] = createSignal("1");
  const [canary, setCanary] = createSignal("0");
  const [pause, setPause] = createSignal("0");
  const [plan, setPlan] = createSignal<{ hosts: RolloutHost[]; skipped: RolloutHost[] }>();
  const [busy, setBusy] = createSignal<"plan" | "start">();
  const [failure, setFailure] = createSignal<string>();

  const selection = () => ({
    hosts: hosts()
      .split(",")
      .map(name => name.trim())
      .filter(name => name !== ""),
    labels: labels().trim(),
    behind: behind(),
  });

  const args = (): Uint8Array =>
    kind() === "host.action" ? cbor.encode({ action: action().trim() }) : new Uint8Array();

  const strategy = () => ({
    batchSize: Number(size()) || 0,
    batchPercent: Number(percent()) || 0,
    failuresAllowed: Number(failures()) || 0,
    canary: Number(canary()) || 0,
    pause: { seconds: BigInt(Number(pause()) || 0), nanos: 0 },
  });

  const problem = (): string | undefined => {
    const chosen = selection();
    if (chosen.hosts.length === 0 && chosen.labels === "" && !chosen.behind) {
      return "Choose the hosts by name, by label, or both those behind their profile.";
    }
    if (kind() === "host.action" && action().trim() === "") return "Name the action the roll-out runs.";
    if (Number(size()) > 0 && Number(percent()) > 0) return "Give a batch size or a percentage, not both.";
    return undefined;
  };

  const runPlan = async (): Promise<void> => {
    const message = problem();
    if (message !== undefined) {
      setFailure(message);
      return;
    }
    setBusy("plan");
    setFailure(undefined);
    try {
      const planned = await shell.rookery.rollouts.plan({ kind: kind(), argsCbor: args(), selection: selection() });
      setPlan({ hosts: planned.hosts, skipped: planned.skipped });
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const start = async (): Promise<void> => {
    const message = problem();
    if (message !== undefined) {
      setFailure(message);
      return;
    }
    setBusy("start");
    setFailure(undefined);
    try {
      const started = await shell.rookery.rollouts.start({
        kind: kind(),
        argsCbor: args(),
        selection: selection(),
        strategy: strategy(),
      });
      if (started.rollout === undefined) throw new Error("The roll-out started but the server did not send it back.");
      props.onStarted(started.rollout.id);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const columns = (): DataColumn<RolloutHost>[] => [
    { id: "host", header: "Host", type: "mono", accessor: host => host.host },
    {
      id: "state",
      header: "Would",
      accessor: host => rolloutHostState(host.state).label,
      cell: host => {
        const shown = rolloutHostState(host.state);
        return <State status={shown.tone}>{shown.label}</State>;
      },
    },
    { id: "batch", header: "Batch", type: "num", accessor: host => `${host.batch}` },
    { id: "why", header: "Why", accessor: host => host.reason },
  ];

  return (
    <Dialog
      open
      onClose={props.onClose}
      title="New roll-out"
      size="lg"
      description="An apply, or one action, on the hosts a selection names, in batches."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy() !== undefined}>
            Cancel
          </Button>
          <Button variant="secondary" onClick={() => void runPlan()} disabled={busy() !== undefined} data-part="plan-rollout">
            {busy() === "plan" ? "Planning…" : "Plan"}
          </Button>
          <Button variant="primary" onClick={() => void start()} disabled={busy() !== undefined} data-part="start-rollout">
            {busy() === "start" ? "Starting…" : "Start"}
          </Button>
        </>
      }
    >
      <div class="nc-stack">
        <Select
          label="Runs"
          value={kind()}
          options={[
            { value: "host.apply", label: "Apply the host profiles" },
            { value: "host.action", label: "One action" },
          ]}
          onChange={chosen => setKind(chosen as "host.apply" | "host.action")}
        />
        <Show when={kind() === "host.action"}>
          <TextField
            label="Action"
            help="A core action such as status, update or reboot, or one a feature declares."
            value={action()}
            placeholder="update"
            onInput={event => setAction(event.currentTarget.value)}
          />
        </Show>
        <SectionHeading title="Selection" level={3} />
        <TextField
          label="Hosts"
          value={hosts()}
          placeholder="studio, forge"
          onInput={event => setHosts(event.currentTarget.value)}
        />
        <TextField
          label="Label selector"
          help="Every criterion is set, so a host must meet all of them."
          value={labels()}
          placeholder="role=desk,site!=lab"
          onInput={event => setLabels(event.currentTarget.value)}
        />
        <Switch label="Only hosts behind their profile" checked={behind()} onChange={setBehind} />
        <SectionHeading title="Batches" level={3} />
        <div class="nc-row">
          <TextField label="Batch size" value={size()} onInput={event => setSize(event.currentTarget.value)} />
          <TextField label="Or percent" value={percent()} placeholder="25" onInput={event => setPercent(event.currentTarget.value)} />
          <TextField label="Failures allowed" value={failures()} onInput={event => setFailures(event.currentTarget.value)} />
        </div>
        <div class="nc-row">
          <TextField
            label="Canary"
            help="Hosts that run first, then wait for Resume."
            value={canary()}
            onInput={event => setCanary(event.currentTarget.value)}
          />
          <TextField
            label="Pause between batches, seconds"
            value={pause()}
            onInput={event => setPause(event.currentTarget.value)}
          />
        </div>
        <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
        <Show when={plan()}>
          {planned => (
            <>
              <SectionHeading title="The plan" level={3} />
              <Show
                when={planned().hosts.length > 0}
                fallback={<Callout tone="warning">The selection names no host that can take it.</Callout>}
              >
                <p class="nc-meta">
                  {planned().hosts.length} {planned().hosts.length === 1 ? "host" : "hosts"} would run.
                </p>
              </Show>
              <DataTable
                bare
                label="Hosts the roll-out would work"
                rows={planned().hosts}
                columns={columns()}
                getRowId={host => host.host}
                empty={{ title: "No host would run" }}
              />
              <Show when={planned().skipped.length > 0}>
                <Callout tone="info">
                  {planned().skipped.length} {planned().skipped.length === 1 ? "host" : "hosts"} would be skipped.
                </Callout>
                <DataTable
                  bare
                  label="Hosts the roll-out would skip"
                  rows={planned().skipped}
                  columns={columns()}
                  getRowId={host => host.host}
                  empty={{ title: "No host would be skipped" }}
                />
              </Show>
            </>
          )}
        </Show>
        <Show when={plan() === undefined}>
          <EmptyState
            size="sm"
            title="Plan before starting"
            description="The plan says which hosts it would work, in which batch, and why it would skip the others."
          />
        </Show>
      </div>
    </Dialog>
  );
}
