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
  IconButton,
  ListRow,
  RowList,
  SectionHeading,
  State,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { errorMessage } from "../../../parts/errors.ts";
import { jobPath } from "../jobs/rows.tsx";
import { hostPath } from "./paths.ts";

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
        shell.navigate(jobPath(execution.value, props.host));
        return;
      }
      if (execution.case === "terminal") {
        const id = execution.value.session?.id ?? "";
        setAsking(undefined);
        if (id !== "") shell.navigate(`${hostPath(props.host, "terminal")}?attach=${encodeURIComponent(id)}`);
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
    <Show when={groups().length > 0}>
    <Card title="Actions" padding="none">
      <Show when={failure()}>{message => <div class="shell-pad"><Callout tone="danger">{message()}</Callout></div>}</Show>
        <For each={groups()}>
          {([group, actions]) => (
            <div class="shell-group" data-action-group={group}>
              <Show when={groups().length > 1}>
                <div class="shell-group__head"><SectionHeading title={group} /></div>
              </Show>
              <RowList label={group}>
                <For each={actions}>
                  {action => (
                    <ListRow
                      data-host-action={action.name}
                      title={action.label === "" ? action.name : action.label}
                      leading={
                        <IconButton
                          size="sm"
                          icon={action.terminal ? "terminal" : "play"}
                          label={`${action.terminal ? "Open" : "Run"} ${action.label === "" ? action.name : action.label}`}
                          disabled={busy() !== undefined}
                          data-part="run-action"
                          onClick={() => run(action)}
                        />
                      }
                      secondary={action.user === 2 ? "as root" : action.user === 3 ? "as the desktop user" : undefined}
                      status={busy() === action.name ? <State status="live">Starting</State> : undefined}
                    />
                  )}
                </For>
              </RowList>
            </div>
          )}
        </For>
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
    </Show>
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
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      </div>
    </Dialog>
  );
}
