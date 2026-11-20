// Roll-outs: an apply, or one action, on a set of hosts in batches. The list is
// what is running and what has run; a new one is planned before it starts, so a
// person sees the hosts it would work and the ones it would skip, and why.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Rollout } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Badge, Button, Callout, Card, Field, IconButton, Page, Select, Spinner } from "../../../ui.tsx";
import { relativeTime, rolloutState, dateOf } from "../format.ts";
import { StateText, Table, type Column } from "../parts.tsx";
import { NewRollout } from "./new.tsx";

const STATES = ["", "running", "paused", "succeeded", "failed", "cancelled"] as const;

/** rolloutPath is a roll-out's page. */
export function rolloutPath(id: string): string {
  return `/rollouts/${encodeURIComponent(id)}`;
}

export function RolloutsPage(): JSX.Element {
  const shell = useShell();
  const [state, setState] = createSignal("");
  const rollouts = createQuery(() => shell.rookery.rollouts.list({ state: state(), limit: 100 }));
  const [starting, setStarting] = createSignal(false);

  onSettled(() =>
    shell.connection.on("rookery/rollouts.changed", () => {
      void rollouts.refresh().catch(() => undefined);
    }),
  );

  const columns: Column<Rollout>[] = [
    {
      id: "runs",
      header: "Runs",
      value: rollout => rollout.kind,
      cell: rollout => (
        <Badge tone="outline">{rollout.kind === "host.action" ? "action" : "apply"}</Badge>
      ),
    },
    {
      id: "state",
      header: "State",
      value: rollout => rolloutState(rollout.pausedBecause === "" ? rollout.state : "paused").label,
      cell: rollout => {
        const shown = rolloutState(rollout.pausedBecause === "" ? rollout.state : "paused");
        return <StateText tone={shown.tone}>{shown.label}</StateText>;
      },
    },
    {
      id: "hosts",
      header: "Hosts",
      num: true,
      value: rollout =>
        `${rollout.succeeded}/${rollout.succeeded + rollout.failed + rollout.pending + rollout.running}`,
    },
    { id: "failed", header: "Failed", num: true, value: rollout => `${rollout.failed}` },
    { id: "started", header: "Started", value: rollout => relativeTime(dateOf(rollout.createdAt)) },
  ];

  const rows = () => rollouts()?.rollouts ?? [];

  return (
    <Page
      title="Roll-outs"
      description="An apply, or one action, on the hosts a selection names, in batches."
      actions={
        <>
          <IconButton label="Read the roll-outs again" glyph="jobs" onClick={() => void rollouts.refresh()} />
          <Button variant="primary" onClick={() => setStarting(true)}>
            New roll-out
          </Button>
        </>
      }
    >
      <Show when={rollouts.error()}>{error => <Callout tone="danger">{error().message}</Callout>}</Show>
      <Card title="Roll-outs">
        <div class="nc-stack">
          <p class="nc-meta">{rows().length} {rows().length === 1 ? "roll-out" : "roll-outs"}</p>
          <Field label="State">
            <Select
              value={state()}
              options={STATES.map(value => ({
                value,
                label: value === "" ? "Every state" : rolloutState(value).label,
              }))}
              onChange={chosen => {
                setState(chosen);
                void rollouts.refresh().catch(() => undefined);
              }}
            />
          </Field>
          <Show when={!rollouts.pending() || rollouts() !== undefined} fallback={<Spinner />}>
            <Table
              label="Roll-outs"
              rows={rows()}
              columns={columns}
              rowKey={rollout => rollout.id}
              onRowClick={rollout => shell.navigate(rolloutPath(rollout.id))}
              empty="No roll-out yet. One applies a profile, or runs one action, in batches."
            />
          </Show>
        </div>
      </Card>
      <Show when={starting()}>
        <NewRollout
          onClose={() => setStarting(false)}
          onStarted={id => {
            setStarting(false);
            shell.navigate(rolloutPath(id));
          }}
        />
      </Show>
    </Page>
  );
}
