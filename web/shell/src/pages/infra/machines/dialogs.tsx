// The dialogs a provisioner needs: adding an entry, running one of its actions,
// and making a machine with it.
//
// An entry names a type an installed plugin declares and the engine it drives: a
// node's, the server's own, or one it reaches itself. A machine is the core's
// kind: a host's machine, made from the seed the server renders.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { HostType, type ProvisionerAction, type ProvisionerEntry, type ProvisionerType } from "@rookery/proto";

import { cbor } from "@rookery/client";
import { Badge, Button, Callout, Dialog, Select, TextField } from "@aiviarium/nocturne";

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

  const choose = (id: string): void => {
    setType(id);
    const found = props.types.find(one => one.id === id);
    setValues(found === undefined ? {} : argumentDefaults(found.arguments));
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
      description="An entry is a name for an engine, from a type an installed plugin declares."
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
        <TextField
          label="Name"
          help="The resource provisioners are granted, and the name a machine names."
          value={name()}
          placeholder="lab-incus"
          onInput={event => setName(event.currentTarget.value)}
        />
        <Select
          label="Type"
          value={type()}
          options={props.types.map(one => ({
            value: one.id,
            label: one.label === "" ? one.id : `${one.id} · ${one.label}`,
          }))}
          onChange={choose}
        />
        <Select
          label="Engine"
          value={engine()}
          options={[
            { value: "local", label: "This server — its own engine" },
            { value: "remote", label: "Another engine — an address and credentials in the arguments" },
            ...props.hosts.map(host => ({ value: host, label: `${host} — the engine on this managed host` })),
          ]}
          onChange={setEngine}
        />
        <Show when={chosen()}>
          {found => (
            <>
              <Show when={found().error !== ""}>
                <Callout tone="warning">{found().error}</Callout>
              </Show>
              <div class="nc-row">
                <For each={found().kinds}>
                  {kind => <Badge tone="outline">{kind.label === "" ? kind.id : kind.label}</Badge>}
                </For>
              </div>
              <Show when={found().secrets.length > 0}>
                <Callout tone="info">
                  It reads the secrets <span class="nc-code">{found().secrets.join(", ")}</span>, which an admin sets
                  for this entry.
                </Callout>
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
      title={`An action on ${props.entry.name}`}
      description="It runs against the entry's engine, and its output is the job's log."
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
                <Callout tone="warning">It changes or removes what the engine holds.</Callout>
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

export function NewMachine(props: {
  entries: readonly ProvisionerEntry[];
  types: readonly ProvisionerType[];
  entry?: string;
  onClose: () => void;
  onStarted: (job: string) => void;
}): JSX.Element {
  const shell = useShell();
  const [entry, setEntry] = createSignal(props.entry ?? props.entries[0]?.name ?? "");
  const [name, setName] = createSignal("");
  const [hostType, setHostType] = createSignal<HostType>(HostType.SSH);
  const [size, setSize] = createSignal("");
  const [image, setImage] = createSignal("");
  const [values, setValues] = createSignal<ArgumentValues>({});
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const chosen = (): ProvisionerEntry | undefined => props.entries.find(one => one.name === entry());
  const type = (): ProvisionerType | undefined => props.types.find(one => one.id === chosen()?.type);
  const makesMachine = (): boolean => type()?.kinds.some(kind => kind.machine) ?? false;

  const submit = async (): Promise<void> => {
    const found = type();
    if (found === undefined || busy()) return;
    setBusy(true);
    setFailure(undefined);
    try {
      const map: Record<string, unknown> = {};
      for (const [key, value] of Object.entries(values())) {
        if (value !== "" && value !== false) map[key] = value;
      }
      if (size() !== "") map["size"] = size();
      if (image() !== "") map["image"] = image();
      const created = await shell.rookery.provisioners.createMachine({
        host: name().trim(),
        provisioner: entry(),
        type: hostType(),
        argsCbor: cbor.encode(map),
      });
      if (created.job !== "") {
        props.onStarted(created.job);
        return;
      }
      throw new Error("The machine was made without a job to follow.");
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
      title="New machine"
      size="lg"
      description="A machine is a host's: the provisioner makes it, and the host's profile sets it up."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button
            variant="primary"
            loading={busy()}
            disabled={name().trim() === "" || !makesMachine()}
            onClick={() => void submit()}
            data-part="create-machine"
          >
            Create machine
          </Button>
        </>
      }
    >
      <div class="nc-stack">
        <Show when={props.entries.length > 0} fallback={<Callout tone="info">Add a provisioner first.</Callout>}>
          <Select
            label="Provisioner"
            value={entry()}
            options={props.entries.map(one => ({ value: one.name, label: `${one.name} · ${one.type}` }))}
            onChange={chosenEntry => {
              setEntry(chosenEntry);
              const found = props.types.find(one => one.id === props.entries.find(each => each.name === chosenEntry)?.type);
              setValues(found === undefined ? {} : argumentDefaults(found.arguments));
              setSize("");
              setImage("");
            }}
          />
          <TextField
            label="Host name"
            help="The machine takes this name, and the host whose profile it is."
            value={name()}
            placeholder="desk-3"
            onInput={event => setName(event.currentTarget.value)}
          />
          <Select
            label="Type"
            help="What the host becomes: the apply sets up the rest."
            value={String(hostType())}
            options={[
              { value: String(HostType.SSH), label: "SSH — reachable over SSH only" },
              { value: String(HostType.MANAGED), label: "Managed — rookeryd, box and the tailnet" },
              { value: String(HostType.WORKSTATION), label: "Workstation — managed, with a desktop" },
            ]}
            onChange={value => setHostType(Number(value) as HostType)}
          />
          <Show when={type()}>
            {found => (
              <>
                <Show when={!makesMachine()}>
                  <Callout tone="warning">
                    {found().id} makes no machine. Its kinds are {found().kinds.map(kind => kind.id).join(", ")}.
                  </Callout>
                </Show>
                <Show when={found().sizes.length > 0}>
                  <Select
                    label="Size"
                    value={size()}
                    options={found().sizes.map(choice => ({
                      value: choice.id,
                      label: `${choice.label === "" ? choice.id : choice.label} — ${choice.cpus} CPUs · ${choice.memoryMb} MB · ${choice.diskGb} GB`,
                    }))}
                    onChange={setSize}
                  />
                </Show>
                <Show when={found().images.length > 0}>
                  <Select
                    label="Image"
                    value={image()}
                    options={found().images.map(choice => ({
                      value: choice.id,
                      label: `${choice.label === "" ? choice.id : choice.label} — ${choice.os} ${choice.release} · ${choice.architecture}`,
                    }))}
                    onChange={setImage}
                  />
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
        </Show>
        <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
        <Callout tone="info">
          The machine's seed has the operator account and the tailnet key; the apply that follows sets everything else
          up.
        </Callout>
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
