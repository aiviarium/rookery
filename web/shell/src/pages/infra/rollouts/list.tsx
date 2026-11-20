// Roll-outs: an apply, or one action, on a set of hosts in batches. The list is
// what is running and what has run; a new one is planned before it starts, so a
// person sees the hosts it would work and the ones it would skip, and why.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Rollout } from "@rookery/proto";
import {
  Badge,
  Callout,
  Card,
  DataTable,
  type DataColumn,
  FilterBar,
  FilterCount,
  Select,
  State,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { relativeTime, rolloutState, dateOf } from "../format.ts";
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

  const rows = () => rollouts()?.rollouts ?? [];

  const columns = (): DataColumn<Rollout>[] => [
    {
      id: "runs",
      header: "Runs",
      accessor: rollout => rollout.kind,
      cell: rollout => <Badge tone="outline">{rollout.kind === "host.action" ? "action" : "apply"}</Badge>,
    },
    {
      id: "state",
      header: "State",
      accessor: rollout => rolloutState(rollout.pausedBecause === "" ? rollout.state : "paused").label,
      cell: rollout => {
        const shown = rolloutState(rollout.pausedBecause === "" ? rollout.state : "paused");
        return <State status={shown.tone}>{shown.label}</State>;
      },
    },
    {
      id: "hosts",
      header: "Hosts",
      type: "num",
      accessor: rollout => `${rollout.succeeded}/${rollout.succeeded + rollout.failed + rollout.pending + rollout.running}`,
    },
    { id: "failed", header: "Failed", type: "num", accessor: rollout => `${rollout.failed}` },
    { id: "started", header: "Started", type: "time", accessor: rollout => relativeTime(dateOf(rollout.createdAt)) },
  ];

  return (
    <Page
      title="Roll-outs"
      description="An apply, or one action, on the hosts a selection names, in batches."
      actions={() => [
        { id: "refresh", label: "Read the roll-outs again", icon: "refresh-cw", onClick: () => void rollouts.refresh() },
        { id: "new", label: "New roll-out", primary: true, onClick: () => setStarting(true) },
      ]}
    >
      <Show when={rollouts.error()}>{error => <Callout tone="danger">{error().message}</Callout>}</Show>
      <Card title="Roll-outs" count={rows().length} padding="none">
        <div class="shell-pad">
          <FilterBar
            label="Roll-out filters"
            end={<FilterCount value={rows().length} unit="Roll-outs" icon="git-pull-request" />}
          >
            <Select
              aria-label="State"
              value={state()}
              options={STATES.map(value => ({
                value,
                label: value === "" ? "Every state" : rolloutState(value).label,
              }))}
              onChange={setState}
            />
          </FilterBar>
        </div>
        <DataTable
          bare
          label="Roll-outs"
          rows={rows()}
          columns={columns()}
          getRowId={rollout => rollout.id}
          loading={rollouts.pending() && rollouts() === undefined}
          onRowClick={rollout => shell.navigate(rolloutPath(rollout.id))}
          empty={{
            title: "No roll-out yet",
            description: "One applies a profile, or runs one action, in batches.",
          }}
        />
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
