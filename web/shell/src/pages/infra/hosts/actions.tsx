// A host's actions: the core ones (status, update, reboot, restart the desktop)
// and those its features declare. Running one is a job, except an action that
// wants a terminal, which opens one on the host.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Action, ActionArgument } from "@rookery/proto";
import { cbor } from "@rookery/client";
import {
  Button,
  Callout,
  Card,
  Dialog,
  EmptyState,
  ListRow,
  RowList,
  SectionHeading,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { errorMessage } from "../format.ts";

/** accountLabel is which account an action runs as. */
export function accountLabel(action: Action): string {
  switch (action.user) {
    case 1:
      return "operator";
    case 2:
      return "root";
    case 3:
      return "desktop";
    default:
      return "operator";
  }
}

export function HostActions(props: { host: string; actions: readonly Action[] }): JSX.Element {
  const shell = useShell();
  const [asking, setAsking] = createSignal<Action>();
  const [busy, setBusy] = createSignal<string>();
  const [failure, setFailure] = createSignal<string>();

  const groups = (): [string, Action[]][] => {
    const byGroup = new Map<string, Action[]>();
    for (const action of props.actions) {
      const key = action.group === "" ? "Host" : action.group;
      const list = byGroup.get(key);
      if (list === undefined) byGroup.set(key, [action]);
      else list.push(action);
    }
    return [...byGroup.entries()];
  };

  const start = async (action: Action, args: Record<string, string>): Promise<void> => {
    if (busy() !== undefined) return;
    setBusy(action.name);
    setFailure(undefined);
    try {
      const ran = await shell.rookery.hosts.runAction({
        name: props.host,
        action: action.name,
        argsCbor: Object.keys(args).length === 0 ? undefined : cbor.encode(args),
      });
      const execution = ran.execution;
      if (execution.case === "job" && execution.value !== "") {
        setAsking(undefined);
        shell.navigate(`/jobs/${encodeURIComponent(execution.value)}`);
        return;
      }
      if (execution.case === "terminal") {
        const id = execution.value.session?.id ?? "";
        setAsking(undefined);
        if (id !== "") shell.navigate(`/terminals/${encodeURIComponent(id)}`);
        return;
      }
      throw new Error("The action started neither a job nor a terminal.");
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const run = (action: Action): void => {
    if (action.arguments.length > 0) {
      setAsking(action);
      return;
    }
    void start(action, {});
  };

  return (
    <Card title="Actions">
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show
        when={groups().length > 0}
        fallback={<EmptyState size="sm" title="No actions" description="This host's type and features declare no actions." />}
      >
        <For each={groups()}>
          {([group, actions]) => (
            <div class="shell-group" data-action-group={group}>
              <SectionHeading title={group} />
              <RowList label={group} variant="inline">
                <For each={actions}>
                  {action => (
                    <ListRow
                      data-host-action={action.name}
                      title={action.label === "" ? action.name : action.label}
                      secondary={`Runs as ${accountLabel(action)}${action.terminal ? " in a terminal" : ""}`}
                      persistentActions={
                        <Button size="sm" disabled={busy() !== undefined} onClick={() => run(action)}>
                          {busy() === action.name ? "Starting…" : "Run"}
                        </Button>
                      }
                    />
                  )}
                </For>
              </RowList>
            </div>
          )}
        </For>
      </Show>
      <Show when={asking()}>
        {action => (
          <RunActionDialog
            host={props.host}
            action={action()}
            onClose={() => setAsking(undefined)}
            onConfirm={(chosen, args) => void start(chosen, args)}
          />
        )}
      </Show>
    </Card>
  );
}

/** RunActionDialog asks for an action's arguments before it runs. */
function RunActionDialog(props: {
  host: string;
  action: Action;
  onClose: () => void;
  onConfirm: (action: Action, args: Record<string, string>) => void;
}): JSX.Element {
  const [values, setValues] = createSignal<Record<string, string>>({});
  const [failure, setFailure] = createSignal<string>();

  const submit = (): void => {
    for (const argument of props.action.arguments) {
      const value = values()[argument.name] ?? argument.defaultValue;
      if (argument.required && value === "") {
        setFailure(`${argument.label === "" ? argument.name : argument.label} is needed.`);
        return;
      }
    }
    const args: Record<string, string> = {};
    for (const argument of props.action.arguments) {
      const value = values()[argument.name] ?? argument.defaultValue;
      if (value !== "") args[argument.name] = value;
    }
    props.onConfirm(props.action, args);
  };

  return (
    <Dialog
      open
      onClose={props.onClose}
      title={props.action.label || props.action.name}
      description={`Runs as ${accountLabel(props.action)} on ${props.host}.`}
      footer={
        <>
          <Button onClick={props.onClose}>Cancel</Button>
          <Button variant="primary" onClick={submit} data-part="run-action-submit">
            {props.action.terminal ? "Open terminal" : "Run action"}
          </Button>
        </>
      }
    >
      <div class="nc-stack">
      <For each={props.action.arguments}>
        {(argument: ActionArgument) => (
          <TextField
            label={argument.label === "" ? argument.name : argument.label}
            value={values()[argument.name] ?? argument.defaultValue}
            onInput={event =>
              setValues(previous => ({ ...previous, [argument.name]: event.currentTarget.value }))
            }
          />
        )}
      </For>
      <Show when={props.action.arguments.length === 0}>
        <p class="nc-muted">It takes no arguments.</p>
      </Show>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      </div>
    </Dialog>
  );
}
