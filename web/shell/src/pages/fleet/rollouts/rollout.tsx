// One roll-out: the hosts it works, the batch each is in, and what became of it.
// Progress arrives on RolloutService.Watch; the header steers it, and the work of
// each host is a child job that opens from its row.
import { For, Show, createSignal, onSettled, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import type { Rollout, RolloutHost } from "@rookery/proto";
import { Button, Callout, Card, EmptyState, ListRow, PropertyList, RowList, Skeleton, State, TextField, type ViewAction } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { FormDialog } from "../../../parts/dialogs.tsx";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, errorMessage, relativeTime, rolloutActive, rolloutHostState, rolloutState } from "../format.ts";
import { rolloutTitle, selectionText } from "./list.tsx";

/** RolloutPage is the roll-out in the path; another one mounts afresh. */
export function RolloutPage(): JSX.Element {
  const params = useParams<{ rollout: string }>();
  return <For each={[params.rollout ?? ""]}>{(id) => <RolloutView id={id} />}</For>;
}

function RolloutView(props: { id: string }): JSX.Element {
  const shell = useShell();
  const id = untrack(() => props.id);
  const [rollout, setRollout] = createSignal<Rollout>();
  const [failure, setFailure] = createSignal<string>();
  const [pausing, setPausing] = createSignal(false);

  onSettled(() => {
    const controller = new AbortController();
    void (async () => {
      try {
        const got = await shell.rookery.rollouts.get({ rollout: id });
        if (got.rollout !== undefined) setRollout(got.rollout);
        for await (const message of shell.rookery.rollouts.watch({ rollout: id }, { signal: controller.signal })) {
          if (message.rollout === undefined) continue;
          setRollout(message.rollout);
          if (!rolloutActive(message.rollout)) break;
        }
      } catch (error) {
        if (!controller.signal.aborted) setFailure(errorMessage(error));
      }
    })();
    return () => controller.abort();
  });

  const steer = createAction(async (what: "resume" | "cancel") => {
    const changed = what === "resume" ? await shell.rookery.rollouts.resume({ rollout: id }) : await shell.rookery.rollouts.cancel({ rollout: id });
    if (changed.rollout !== undefined) setRollout(changed.rollout);
  }, { failed: "The roll-out did not change" });

  const state = () => {
    const current = rollout();
    return current === undefined ? undefined : rolloutState(current.pausedBecause === "" ? current.state : "paused");
  };
  const hosts = (): RolloutHost[] => {
    const current = rollout();
    if (current === undefined) return [];
    return [...current.hosts].sort((a, b) => a.batch - b.batch || a.host.localeCompare(b.host));
  };

  const actions = (): ViewAction[] => {
    const current = rollout();
    const list: ViewAction[] = [];
    if (rolloutActive(current)) {
      list.push({ id: "cancel", label: "Cancel roll-out", icon: "close", overflow: true, danger: true, disabled: steer.busy(), onClick: () => void steer.run("cancel") });
    }
    if (current?.state === "running") list.push({ id: "pause", label: "Pause", icon: "pause", onClick: () => setPausing(true) });
    if (current?.state === "paused") {
      list.push({ id: "resume", label: "Resume", icon: "play", disabled: steer.busy(), onClick: () => void steer.run("resume") });
    }
    return list;
  };

  return (
    <View
      id="rollout"
      crumbs={[{ label: "Roll-outs", href: "/rollouts" }, { label: rollout() === undefined ? id.slice(0, 10) : rolloutTitle(rollout()!) }]}
      context={state() === undefined ? undefined : <State status={state()!.tone}>{state()!.label}</State>}
      actions={actions()}
      error={failure()}
      aside={<Show when={rollout()}>{(current) => <Progress rollout={current()} />}</Show>}
      asideLabel="Progress"
    >
      <Show when={rollout()?.pausedBecause}>
        {(reason) => (
          <Callout
            tone="warning"
            action={
              <Show when={rollout()?.state === "paused"}>
                <Button size="sm" onClick={() => void steer.run("resume")}>
                  Resume
                </Button>
              </Show>
            }
          >
            Paused: {reason()}
          </Callout>
        )}
      </Show>
      <Show when={rollout()} fallback={<Skeleton width="50%" />}>
        {(current) => (
          <>
            <Card title="Hosts" count={hosts().length} padding="none">
              <Show when={hosts().length > 0} fallback={<EmptyState size="sm" title="No hosts" />}>
                <RowList label="Hosts of the roll-out">
                  <For each={hosts()} keyed={(host) => host.host}>
                    {(host) => {
                      const shown = () => rolloutHostState(host().state);
                      return (
                        <ListRow
                          icon="server"
                          title={host().host}
                          secondary={`Batch ${host().batch}`}
                          detail={host().error || undefined}
                          status={<State status={shown().tone}>{shown().label}</State>}
                          href={host().job === "" ? undefined : `/jobs/${encodeURIComponent(host().job)}`}
                        />
                      );
                    }}
                  </For>
                </RowList>
              </Show>
            </Card>
            <Show when={current().skipped.length > 0}>
              <Card title="Skipped" count={current().skipped.length} padding="none">
                <RowList label="Skipped hosts">
                  <For each={current().skipped} keyed={(host) => host.host}>
                    {(host) => <ListRow icon="server" muted title={host().host} secondary={host().reason || host().error} href={`/hosts/${encodeURIComponent(host().host)}`} />}
                  </For>
                </RowList>
              </Card>
            </Show>
          </>
        )}
      </Show>
      <Show when={pausing()}>
        <PauseDialog id={id} onClose={() => setPausing(false)} onPaused={setRollout} />
      </Show>
    </View>
  );
}

function Progress(props: { rollout: Rollout }): JSX.Element {
  return (
    <div class="shell-view">
      <PropertyList
        items={[
          { label: "Runs", value: rolloutTitle(props.rollout) },
          { label: "Hosts", value: selectionText(props.rollout.selection) },
          { label: "Succeeded", value: String(props.rollout.succeeded) },
          { label: "Running", value: String(props.rollout.running) },
          { label: "Pending", value: String(props.rollout.pending) },
          { label: "Failed", value: String(props.rollout.failed) },
          { label: "Started", value: relativeTime(dateOf(props.rollout.startedAt ?? props.rollout.createdAt)) || "Not yet" },
          { label: "Finished", value: relativeTime(dateOf(props.rollout.finishedAt)) || "Not yet" },
          { label: "By", value: <span class="nc-mono">{props.rollout.createdBy}</span> },
        ]}
      />
    </div>
  );
}

function PauseDialog(props: { id: string; onClose: () => void; onPaused: (rollout: Rollout) => void }): JSX.Element {
  const shell = useShell();
  const [why, setWhy] = createSignal("");
  const pause = createAction(async () => {
    const changed = await shell.rookery.rollouts.pause({ rollout: props.id, why: why().trim() });
    if (changed.rollout !== undefined) props.onPaused(changed.rollout);
  });
  return (
    <FormDialog
      open
      title="Pause the roll-out?"
      submit="Pause"
      action={pause}
      onClose={props.onClose}
    >
      <TextField
        label="Why"
        info="Shown on the roll-out while it waits. Running hosts finish their work, and no new batch starts until it is resumed."
        placeholder="Checking the first batch"
        value={why()}
        onInput={(event) => setWhy(event.currentTarget.value)}
      />
    </FormDialog>
  );
}
