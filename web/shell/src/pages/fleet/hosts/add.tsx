// "Add host": an existing machine Rookery reaches over SSH, or a new machine a
// provisioner makes, which becomes a host when its job is done.
//
// For an existing machine, a password or a private key given here is used once,
// by first contact, to install the server's own key; it is never stored. A new
// machine needs only its name, what it becomes, and the size and image its
// provisioner offers: the provisioner already knows its engine.
import { Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { HostType, type ProvisionerEntry, type ProvisionerType } from "@rookery/proto";
import { Select, TextArea, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { FormDialog } from "../../../parts/dialogs.tsx";
import { useShell } from "../../../shell.tsx";

/** CredentialKind is how first contact logs in when the key ring's key is refused. */
type CredentialKind = "none" | "password" | "key";

/** EXISTING is the choice of a machine that is there already. */
const EXISTING = "";

/** NAME is what a host's name may be. */
const NAME = /^[a-z0-9][a-z0-9-]*$/;

/** TYPES are what a host can be. */
const TYPES = [
  { value: String(HostType.MANAGED), label: "Managed", description: "rookeryd, box and the tailnet" },
  { value: String(HostType.WORKSTATION), label: "Workstation", description: "Managed, with a desktop" },
  { value: String(HostType.SSH), label: "SSH", description: "Reached over SSH only" },
];

/** Added is what adding gave: the host, and the job to follow when there is one. */
export interface Added {
  readonly name: string;
  readonly job: string;
}

export function AddHost(props: {
  /** entries are the provisioners that make machines. */
  entries: readonly ProvisionerEntry[];
  types: readonly ProvisionerType[];
  /** from is the provisioner to start with, or none for an existing machine. */
  from?: string;
  onClose: () => void;
  onAdded: (added: Added) => void;
}): JSX.Element {
  const shell = useShell();
  const [from, setFrom] = createSignal(untrack(() => props.from ?? EXISTING));
  const [name, setName] = createSignal("");
  const [type, setType] = createSignal(String(HostType.MANAGED));
  // An existing machine.
  const [address, setAddress] = createSignal("");
  const [user, setUser] = createSignal("ops");
  const [port, setPort] = createSignal("22");
  const [kind, setKind] = createSignal<CredentialKind>("none");
  const [password, setPassword] = createSignal("");
  const [key, setKey] = createSignal("");
  // A new machine.
  const [size, setSize] = createSignal("");
  const [image, setImage] = createSignal("");

  const entry = () => props.entries.find((one) => one.name === from());
  const provisioner = () => props.types.find((one) => one.id === entry()?.type);
  const existing = () => entry() === undefined;

  const nameProblem = () =>
    name() !== "" && !NAME.test(name().trim()) ? "Lowercase letters, digits and hyphens" : undefined;
  const portProblem = () => {
    const value = Number(port());
    return Number.isInteger(value) && value > 0 && value <= 65535 ? undefined : "1 to 65535";
  };
  const complete = () => {
    if (!NAME.test(name().trim())) return false;
    if (!existing()) return true;
    return (
      address().trim() !== "" &&
      portProblem() === undefined &&
      (kind() !== "password" || password() !== "") &&
      (kind() !== "key" || key().trim() !== "")
    );
  };

  const add = createAction(async () => {
    const host = name().trim();
    if (!existing()) {
      const args: Record<string, unknown> = {};
      if (size() !== "") args["size"] = size();
      if (image() !== "") args["image"] = image();
      const created = await shell.rookery.provisioners.createMachine({
        host,
        provisioner: from(),
        type: Number(type()) as HostType,
        argsCbor: cbor.encode(args),
      });
      props.onAdded({ name: host, job: created.job });
      return;
    }
    const added = await shell.rookery.hosts.add({
      name: host,
      address: address().trim(),
      type: Number(type()) as HostType,
      user: user().trim(),
      port: Number(port()),
      credential:
        kind() === "password"
          ? { kind: { case: "password", value: password() } }
          : kind() === "key"
            ? { kind: { case: "privateKey", value: new TextEncoder().encode(key()) } }
            : undefined,
    });
    props.onAdded({ name: added.host?.name ?? host, job: added.firstContact });
  });

  return (
    <FormDialog
      open
      title="Add host"
      submit={existing() ? "Add host" : "Create machine"}
      action={add}
      disabled={!complete()}
      onClose={props.onClose}
    >
      <Show when={props.entries.length > 0}>
        <Select
          label="Machine"
          value={from()}
          options={[
            { value: EXISTING, label: "An existing machine", description: "Over SSH" },
            ...props.entries.map((one) => ({
              value: one.name,
              label: `New on ${one.name}`,
              description: props.types.find((each) => each.id === one.type)?.label || one.type,
            })),
          ]}
          onChange={(next) => {
            setFrom(next);
            setSize("");
            setImage("");
          }}
        />
      </Show>
      <TextField
        label="Name"
        info="Commands and grants name the host by it"
        error={nameProblem()}
        value={name()}
        placeholder="studio"
        onInput={(event) => setName(event.currentTarget.value)}
      />
      <Select label="Type" value={type()} options={TYPES} onChange={setType} />
      <Show
        when={existing()}
        fallback={
          <>
            <Show when={(provisioner()?.sizes.length ?? 0) > 0}>
              <Select
                label="Size"
                placeholder="Default"
                value={size()}
                options={(provisioner()?.sizes ?? []).map((choice) => ({
                  value: choice.id,
                  label: choice.label || choice.id,
                  description: `${choice.cpus} CPUs · ${choice.memoryMb} MB · ${choice.diskGb} GB`,
                }))}
                onChange={setSize}
              />
            </Show>
            <Show when={(provisioner()?.images.length ?? 0) > 0}>
              <Select
                label="Image"
                placeholder="Default"
                value={image()}
                options={(provisioner()?.images ?? []).map((choice) => ({
                  value: choice.id,
                  label: choice.label || choice.id,
                  description: `${choice.os} ${choice.release} · ${choice.architecture}`,
                }))}
                onChange={setImage}
              />
            </Show>
          </>
        }
      >
        <div class="shell-form__row">
          <TextField
            label="Address"
            mono
            value={address()}
            placeholder="192.0.2.10"
            onInput={(event) => setAddress(event.currentTarget.value)}
          />
          <TextField label="SSH user" mono value={user()} onInput={(event) => setUser(event.currentTarget.value)} />
          <TextField
            label="Port"
            mono
            error={portProblem()}
            value={port()}
            onInput={(event) => setPort(event.currentTarget.value)}
          />
        </div>
        <Select
          label="First contact"
          info="First contact installs the server's key on the machine. A password or key given here is used once and not stored."
          value={kind()}
          options={[
            { value: "none", label: "Key installed already" },
            { value: "password", label: "Password, once" },
            { value: "key", label: "Private key, once" },
          ]}
          onChange={(value) => setKind(value as CredentialKind)}
        />
        <Show when={kind() === "password"}>
          <TextField
            label="Password"
            type="password"
            value={password()}
            onInput={(event) => setPassword(event.currentTarget.value)}
          />
        </Show>
        <Show when={kind() === "key"}>
          <TextArea
            label="Private key"
            info="PEM, unencrypted"
            rows={5}
            value={key()}
            onInput={(event) => setKey(event.currentTarget.value)}
          />
        </Show>
      </Show>
    </FormDialog>
  );
}
