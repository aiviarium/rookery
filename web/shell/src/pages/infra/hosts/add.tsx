// "Add host": a machine Rookery reaches over SSH. A password or a private key
// given here is used once, by first contact, to install the server's own key; it
// is never stored.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { HostType } from "@rookery/proto";
import { Select, TextArea, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { FormDialog } from "../../../parts/dialogs.tsx";
import { useShell } from "../../../shell.tsx";

/** CredentialKind is how first contact logs in when the key ring's key is refused. */
type CredentialKind = "none" | "password" | "key";

/** NAME is what a host's name may be. */
const NAME = /^[a-z0-9][a-z0-9-]*$/;

export function AddHost(props: { onClose: () => void; onAdded: (name: string, firstContact: string) => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const [address, setAddress] = createSignal("");
  const [type, setType] = createSignal(String(HostType.MANAGED));
  const [user, setUser] = createSignal("ops");
  const [port, setPort] = createSignal("22");
  const [kind, setKind] = createSignal<CredentialKind>("none");
  const [password, setPassword] = createSignal("");
  const [key, setKey] = createSignal("");

  const nameProblem = () =>
    name() !== "" && !NAME.test(name().trim()) ? "Lowercase letters, digits and hyphens." : undefined;
  const portProblem = () => {
    const value = Number(port());
    return Number.isInteger(value) && value > 0 && value <= 65535 ? undefined : "1 to 65535.";
  };
  const complete = () =>
    NAME.test(name().trim()) && address().trim() !== "" && portProblem() === undefined &&
    (kind() !== "password" || password() !== "") && (kind() !== "key" || key().trim() !== "");

  const add = createAction(async () => {
    const added = await shell.rookery.hosts.add({
      name: name().trim(),
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
    props.onAdded(added.host?.name ?? name().trim(), added.firstContact);
  });

  return (
    <FormDialog
      open
      title="Add host"
      description="Rookery reaches the machine over SSH. First contact installs the server's key there, so later links need no password."
      submit="Add host"
      action={add}
      disabled={!complete()}
      onClose={props.onClose}
    >
      <TextField
        label="Name"
        help="Commands and grants name the host by it."
        error={nameProblem()}
        value={name()}
        placeholder="studio"
        onInput={(event) => setName(event.currentTarget.value)}
      />
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
        label="Type"
        value={type()}
        options={[
          { value: String(HostType.MANAGED), label: "Managed: rookeryd, box and the tailnet" },
          { value: String(HostType.WORKSTATION), label: "Workstation: a managed host with a desktop" },
          { value: String(HostType.SSH), label: "SSH: reached over SSH only" },
        ]}
        onChange={setType}
      />
      <Select
        label="First contact"
        value={kind()}
        options={[
          { value: "none", label: "The server's key is installed already" },
          { value: "password", label: "Log in once with a password" },
          { value: "key", label: "Log in once with a private key" },
        ]}
        onChange={(value) => setKind(value as CredentialKind)}
      />
      <Show when={kind() === "password"}>
        <TextField
          label="Password"
          type="password"
          help="Used once to install the server's key. Not stored."
          value={password()}
          onInput={(event) => setPassword(event.currentTarget.value)}
        />
      </Show>
      <Show when={kind() === "key"}>
        <TextArea
          label="Private key"
          help="PEM, unencrypted. Used once to install the server's key. Not stored."
          rows={5}
          value={key()}
          onInput={(event) => setKey(event.currentTarget.value)}
        />
      </Show>
    </FormDialog>
  );
}
