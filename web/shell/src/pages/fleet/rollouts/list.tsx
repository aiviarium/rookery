// Roll-outs: an apply, or one action, on a set of hosts in batches. The list is
// what is running and what has run; a new one is planned before it starts, so a
// person sees the hosts it would work and the ones it would skip, and why.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Rollout, RolloutSelection } from "@rookery/proto";
import { Button, Card, EmptyState, FilterBar, ListRow, RowList, Select, Skeleton, State } from "@aiviarium/nocturne";

import { createLive } from "../../../parts/data.ts";
import { errorMessage } from "../../../parts/errors.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, relativeTime, rolloutState } from "../format.ts";
import { NewRollout } from "./new.tsx";

const STATES = ["", "running", "paused", "succeeded", "failed", "cancelled"] as const;

/** rolloutPath is a roll-out's page. */
export function rolloutPath(id: string): string {
  return `/rollouts/${encodeURIComponent(id)}`;
}

/** rolloutTitle is what a roll-out runs, in words. */
export function rolloutTitle(rollout: Rollout): string {
  return rollout.kind === "host.action" ? "Action" : "Apply";
}

/** selectionText is which hosts a selection names, in words. */
export function selectionText(selection: RolloutSelection | undefined): string {
  if (selection === undefined) return "Every host";
  const parts: string[] = [];
  if (selection.hosts.length > 0) parts.push(selection.hosts.length <= 3 ? selection.hosts.join(", ") : `${selection.hosts.length} hosts`);
  if (selection.labels !== "") parts.push(selection.labels);
  if (selection.behind) parts.push("behind their profile");
  return parts.length === 0 ? "Every host" : parts.join(" · ");
}

export function RolloutsPage(): JSX.Element {
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ state?: string }>();
  const state = () => search.state ?? "";
  const rollouts = createLive(() => shell.rookery.rollouts.list({ state: state(), limit: 100 }), {
    key: state,
    events: ["rookery/rollouts."],
  });
  const [starting, setStarting] = createSignal(false);
  const rows = () => rollouts()?.rollouts ?? [];

  return (
    <View
      id="rollouts"
      crumbs={[{ label: "Roll-outs" }]}
      error={rollouts.error() === undefined ? undefined : `The roll-outs could not be read. ${errorMessage(rollouts.error())}`}
      actions={[
        { id: "new", label: "New roll-out", icon: "plus", onClick: () => setStarting(true) },
      ]}
    >
      <FilterBar label="Roll-out filters">
        <Select
          aria-label="State"
          value={state()}
          options={STATES.map((value) => ({ value, label: value === "" ? "Every state" : rolloutState(value).label }))}
          onChange={(next) => setSearch({ state: next === "" ? undefined : next })}
        />
      </FilterBar>
      <Show when={rollouts()} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState
                icon="branch"
                title={state() === "" ? "No roll-outs" : "No roll-out matches"}
              >
                <Button icon="plus" onClick={() => setStarting(true)}>
                  New roll-out
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Roll-outs">
              <For each={rows()} keyed={(rollout) => rollout.id}>
                {(rollout) => {
                  const shown = () => rolloutState(rollout().pausedBecause === "" ? rollout().state : "paused");
                  const total = () => rollout().succeeded + rollout().failed + rollout().pending + rollout().running;
                  return (
                    <ListRow
                      data-rollout={rollout().id}
                      icon="branch"
                      title={rolloutTitle(rollout())}
                      secondary={selectionText(rollout().selection)}
                      detail={rollout().failed > 0 ? `${rollout().failed} failed` : undefined}
                      status={<State status={shown().tone}>{shown().label}</State>}
                      meta={`${rollout().succeeded}/${total()} hosts · ${relativeTime(dateOf(rollout().createdAt))}`}
                      href={rolloutPath(rollout().id)}
                    />
                  );
                }}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>
      <Show when={starting()}>
        <NewRollout
          onClose={() => setStarting(false)}
          onStarted={(id) => {
            setStarting(false);
            shell.navigate(rolloutPath(id));
          }}
        />
      </Show>
    </View>
  );
}
