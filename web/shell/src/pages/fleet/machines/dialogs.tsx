// The dialogs a provisioner needs: adding one and running one of its actions.
// Making a machine is adding a host, in hosts/add.tsx.
//
// An entry names a type an installed plugin declares and the engine it drives: a
// managed host's, the server's own, or one it reaches itself with the address
// and credentials in its arguments.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { ProvisionerAction, ProvisionerEntry, ProvisionerType } from "@rookery/proto";

import { Button, Callout, Dialog, Select, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { errorMessage } from "../format.ts";
import {
  ArgumentFields,
  type ArgumentValues,
  argumentDefaults,
  argumentProblems,
  argumentsCbor,
} from "./arguments.tsx";

export function NewProvisioner(props: {
  types: readonly ProvisionerType[];
  hosts: readonly string[];
  onClose: () => void;
  onAdded: () => void;
}): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const [type, setType] = createSignal("");
  const [engine, setEngine] = createSignal("local");
  const [values, setValues] = createSignal<ArgumentValues>({});
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const chosen = (): ProvisionerType | undefined => props.types.find(one => one.id === type());

  // Where the engine may be, as the type says: the server's own, a managed host's, or one it reaches itself.
  const engines = (): Array<{ value: string; label: string; description?: string }> => {
    const found = chosen();
    if (found === undefined) return [];
    return [
      ...(found.local ? [{ value: "local", label: "This server" }] : []),
      ...(found.node ? props.hosts.map(host => ({ value: host, label: host, description: "Managed host" })) : []),
      { value: "remote", label: "Elsewhere", description: "Reached with the arguments below" },
    ];
  };

  const choose = (id: string): void => {
    setType(id);
    const found = props.types.find(one => one.id === id);
    setValues(found === undefined ? {} : argumentDefaults(found.arguments));
    setEngine(found?.local ? "local" : found?.node && props.hosts[0] !== undefined ? props.hosts[0] : "remote");
  };

  const submit = async (): Promise<void> => {
    const found = chosen();
    if (found === undefined || busy()) return;
    if (Object.keys(argumentProblems(found.arguments, values())).length > 0) {
      setFailure("Some arguments are needed.");
      return;
    }
    setBusy(true);
    setFailure(undefined);
    try {
      await shell.rookery.provisioners.add({
        name: name().trim(),
        type: found.id,
        argsCbor: argumentsCbor(found.arguments, values()),
        node: engine() === "local" || engine() === "remote" ? "" : engine(),
        remote: engine() === "remote" ? "remote" : "",
      });
      props.onAdded();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={props.onClose}
      title="New provisioner"
      size="lg"
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button
            variant="primary"
            loading={busy()}
            disabled={name().trim() === "" || type() === ""}
            onClick={() => void submit()}
            data-part="add-provisioner"
          >
            Add provisioner
          </Button>
        </>
      }
    >
      <div class="nc-stack">
        <Select
          label="Type"
          placeholder="Choose a type"
          value={type()}
          options={props.types.map(one => ({
            value: one.id,
            label: one.label === "" ? one.id : one.label,
            description: one.label === "" ? undefined : one.id,
          }))}
          onChange={choose}
        />
        <TextField
          label="Name"
          info="Grants and machines name the provisioner by it"
          value={name()}
          placeholder="lab-incus"
          onInput={event => setName(event.currentTarget.value)}
        />
        <Show when={engines().length > 1}>
          <Select label="Engine" value={engine()} options={engines()} onChange={setEngine} />
        </Show>
        <Show when={chosen()}>
          {found => (
            <>
              <Show when={found().error !== ""}>
                <Callout tone="warning">{found().error}</Callout>
              </Show>
              <ArgumentFields
                arguments={found().arguments}
                values={values()}
                errors={{}}
                onChange={(key, value) => setValues(previous => ({ ...previous, [key]: value }))}
              />
            </>
          )}
        </Show>
        <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      </div>
    </Dialog>
  );
}

export function RunProvisionerAction(props: {
  entry: ProvisionerEntry;
  type: ProvisionerType | undefined;
  onClose: () => void;
  onStarted: (job: string) => void;
}): JSX.Element {
  const shell = useShell();
  const [action, setAction] = createSignal<ProvisionerAction>();
  const [values, setValues] = createSignal<ArgumentValues>({});
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const choose = (name: string): void => {
    const found = props.type?.actions.find(one => one.name === name);
    setAction(found);
    setValues(found === undefined ? {} : argumentDefaults(found.arguments));
  };

  const submit = async (): Promise<void> => {
    const chosen = action();
    if (chosen === undefined || busy()) return;
    if (Object.keys(argumentProblems(chosen.arguments, values())).length > 0) {
      setFailure("Some arguments are needed.");
      return;
    }
    setBusy(true);
    setFailure(undefined);
    try {
      const stream = shell.rookery.provisioners.runAction({
        provisioner: props.entry.name,
        action: chosen.name,
        argsCbor: argumentsCbor(chosen.arguments, values()),
      });
      for await (const message of stream) {
        if (message.job !== "") {
          props.onStarted(message.job);
          return;
        }
      }
      throw new Error("The action ran without starting a job.");
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={props.onClose}
      title={`Run an action on ${props.entry.name}`}
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button
            variant="primary"
            loading={busy()}
            disabled={action() === undefined}
            onClick={() => void submit()}
          >
            Run action
          </Button>
        </>
      }
    >
      <div class="nc-stack">
        <Select
          label="Action"
          value={action()?.name ?? ""}
          options={(props.type?.actions ?? []).map(one => ({
            value: one.name,
            label: one.label === "" ? one.name : one.label,
          }))}
          onChange={choose}
        />
        <Show when={action()}>
          {chosen => (
            <>
              <Show when={chosen().destructive}>
                <Callout tone="warning">Changes or removes what the engine holds</Callout>
              </Show>
              <ArgumentFields
                arguments={chosen().arguments}
                values={values()}
                errors={{}}
                onChange={(key, value) => setValues(previous => ({ ...previous, [key]: value }))}
              />
            </>
          )}
        </Show>
        <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      </div>
    </Dialog>
  );
}

/** engineName is where an entry's engine is, as one word. */
export function engineName(entry: ProvisionerEntry): string {
  if (entry.node !== "") return entry.node;
  if (entry.remote !== "") return entry.remote;
  return "this server";
}
