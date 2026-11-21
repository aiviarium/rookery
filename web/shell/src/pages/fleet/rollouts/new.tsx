// "New roll-out": choose what runs, which hosts it runs on, and how it is split
// into batches. The plan follows the selection as it is typed, so the dialog
// always shows the hosts it would work and the ones it would skip, and why, and
// a selection that names nothing cannot start.
import { For, Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import type { RolloutHost } from "@rookery/proto";
import { Badge, Button, Callout, Dialog, ListRow, RowList, Skeleton, State, Switch, Tabs, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { useShell } from "../../../shell.tsx";
import { errorMessage, rolloutHostState } from "../format.ts";

type Kind = "host.apply" | "host.action";

export function NewRollout(props: { onClose: () => void; onStarted: (id: string) => void }): JSX.Element {
  const shell = useShell();
  const [kind, setKind] = createSignal<Kind>("host.apply");
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
  const [planError, setPlanError] = createSignal<string>();

  const selection = () => ({
    hosts: hosts()
      .split(",")
      .map((name) => name.trim())
      .filter((name) => name !== ""),
    labels: labels().trim(),
    behind: behind(),
  });
  const args = (): Uint8Array => (kind() === "host.action" ? cbor.encode({ action: action().trim() }) : new Uint8Array());
  const chosen = () => {
    const now = selection();
    return now.hosts.length > 0 || now.labels !== "" || now.behind;
  };
  const problem = (): string | undefined => {
    if (kind() === "host.action" && action().trim() === "") return "Name the action the roll-out runs.";
    if (Number(size()) > 0 && Number(percent()) > 0) return "Give a batch size or a percentage, not both.";
    return undefined;
  };

  // The plan follows the selection, a moment after the typing stops.
  createEffect(
    () => (chosen() ? JSON.stringify([kind(), action().trim(), selection()]) : ""),
    (key) => {
      setPlan(undefined);
      setPlanError(undefined);
      if (key === "") return;
      const [planKind, , planSelection] = JSON.parse(key) as [Kind, string, ReturnType<typeof selection>];
      const planArgs = args();
      let stale = false;
      const timer = setTimeout(() => {
        shell.rookery.rollouts
          .plan({ kind: planKind, argsCbor: planArgs, selection: planSelection })
          .then((planned) => {
            if (!stale) setPlan({ hosts: planned.hosts, skipped: planned.skipped });
          })
          .catch((error: unknown) => {
            if (!stale) setPlanError(errorMessage(error));
          });
      }, 350);
      return () => {
        stale = true;
        clearTimeout(timer);
      };
    },
  );

  const start = createAction(async () => {
    const started = await shell.rookery.rollouts.start({
      kind: kind(),
      argsCbor: args(),
      selection: selection(),
      strategy: {
        batchSize: Number(size()) || 0,
        batchPercent: Number(percent()) || 0,
        failuresAllowed: Number(failures()) || 0,
        canary: Number(canary()) || 0,
        pause: { seconds: BigInt(Number(pause()) || 0), nanos: 0 },
      },
    });
    if (started.rollout === undefined) throw new Error("The roll-out started but the server did not send it back.");
    props.onStarted(started.rollout.id);
  });

  const ready = () => problem() === undefined && (plan()?.hosts.length ?? 0) > 0;

  return (
    <Dialog
      open
      onClose={props.onClose}
      closeOnBackdrop={false}
      title="New roll-out"
      size="lg"
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose} disabled={start.busy()}>
            Cancel
          </Button>
          <Button variant="primary" loading={start.busy()} disabled={!ready()} onClick={() => void start.run()} data-part="start-rollout">
            {plan() === undefined || plan()!.hosts.length === 0 ? "Start" : `Start on ${plan()!.hosts.length} ${plan()!.hosts.length === 1 ? "host" : "hosts"}`}
          </Button>
        </>
      }
    >
      <div class="shell-form">
        <Tabs
          label="Runs"
          value={kind()}
          onChange={(next) => setKind(next as Kind)}
          items={[
            { id: "host.apply", label: "Apply the profiles" },
            { id: "host.action", label: "Run one action" },
          ]}
        />
        <Show when={kind() === "host.action"}>
          <TextField
            label="Action"
            mono
            info="A core action such as status, update or reboot, or one a feature declares."
            placeholder="update"
            value={action()}
            onInput={(event) => setAction(event.currentTarget.value)}
          />
        </Show>
        <div class="shell-form__row">
          <TextField label="Hosts" mono placeholder="studio, forge" value={hosts()} onInput={(event) => setHosts(event.currentTarget.value)} />
          <TextField
            label="Labels"
            mono
            placeholder="role=desk,site!=lab"
            info="A host must match every one."
            value={labels()}
            onInput={(event) => setLabels(event.currentTarget.value)}
          />
        </div>
        <Switch label="Only hosts behind their profile" checked={behind()} onChange={setBehind} />
        <div class="shell-form__row">
          <TextField label="Batch size" value={size()} onInput={(event) => setSize(event.currentTarget.value)} />
          <TextField label="Or a percentage" placeholder="25" value={percent()} onInput={(event) => setPercent(event.currentTarget.value)} />
          <TextField label="Failures allowed" value={failures()} onInput={(event) => setFailures(event.currentTarget.value)} />
        </div>
        <div class="shell-form__row">
          <TextField
            label="Canary hosts"
            info="They run first, then the roll-out waits for Resume."
            value={canary()}
            onInput={(event) => setCanary(event.currentTarget.value)}
          />
          <TextField label="Pause between batches (s)" value={pause()} onInput={(event) => setPause(event.currentTarget.value)} />
        </div>
        <Show when={problem()}>{(message) => <Callout tone="warning">{message()}</Callout>}</Show>
        <Show when={planError()}>{(message) => <Callout tone="danger">{message()}</Callout>}</Show>
        <Show when={start.error()}>{(message) => <Callout tone="danger">{message()}</Callout>}</Show>
        <Show when={chosen()} fallback={null}>
          <Show when={plan()} fallback={<Show when={planError() === undefined}><Skeleton width="40%" /></Show>}>
            {(planned) => (
              <Show
                when={planned().hosts.length + planned().skipped.length > 0}
                fallback={<Callout tone="warning">The selection names no host.</Callout>}
              >
                <RowList label="The plan" variant="inline" data-part="rollout-plan">
                  <For each={[...planned().hosts, ...planned().skipped]} keyed={(host) => host.host}>
                    {(host) => {
                      const shown = () => rolloutHostState(host().state);
                      return (
                        <ListRow
                          icon="server"
                          title={host().host}
                          secondary={host().reason || undefined}
                          muted={host().batch === 0 && host().reason !== ""}
                          status={<State status={shown().tone}>{shown().label}</State>}
                          meta={host().batch > 0 ? <Badge tone="outline">{`batch ${host().batch}`}</Badge> : undefined}
                        />
                      );
                    }}
                  </For>
                </RowList>
              </Show>
            )}
          </Show>
        </Show>
      </div>
    </Dialog>
  );
}
