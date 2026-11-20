// "Add host": a machine Rookery reaches over SSH. A password or a private key
// given here is used once, by first contact, to install the server's own key; it
// is never stored.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { HostType, type Host } from "@rookery/proto";
import { Button, Callout, Dialog, Select, TextArea, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { errorMessage } from "../format.ts";

/** CredentialKind is how first contact logs in when the key ring's key is refused. */
type CredentialKind = "none" | "password" | "key";

export function AddHost(props: {
  onClose: () => void;
  onAdded: (host: Host, firstContact: string) => void;
}): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const [address, setAddress] = createSignal("");
  const [type, setType] = createSignal(String(HostType.SSH));
  const [user, setUser] = createSignal("ops");
  const [port, setPort] = createSignal("22");
  const [kind, setKind] = createSignal<CredentialKind>("none");
  const [password, setPassword] = createSignal("");
  const [key, setKey] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const problem = (): string | undefined => {
    if (name().trim() === "") return "The host needs a name.";
    if (!/^[a-z0-9][a-z0-9-]*$/.test(name().trim())) return "A name is lowercase letters, digits and hyphens.";
    if (address().trim() === "") return "The host needs an address to reach it at.";
    const sshPort = Number(port());
    if (!Number.isInteger(sshPort) || sshPort <= 0 || sshPort > 65535) return "The port is 1 to 65535.";
    return undefined;
  };

  const submit = async (): Promise<void> => {
    if (busy()) return;
    const message = problem();
    if (message !== undefined) {
      setFailure(message);
      return;
    }
    setBusy(true);
    setFailure(undefined);
    try {
      const added = await shell.rookery.hosts.add({
        name: name().trim(),
        address: address().trim(),
        type: Number(type()) as HostType,
        user: user().trim(),
        port: Number(port()),
        credential:
          kind() === "password" && password() !== ""
            ? { kind: { case: "password", value: password() } }
            : kind() === "key" && key() !== ""
              ? { kind: { case: "privateKey", value: new TextEncoder().encode(key()) } }
              : undefined,
      });
      if (added.host === undefined) throw new Error("The server added the host but did not send it back.");
      props.onAdded(added.host, added.firstContact);
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
      title="Add host"
      size="md"
      description="Rookery reaches the machine over SSH. First contact installs the server's key there, so later links need no password."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button variant="primary" loading={busy()} onClick={() => void submit()} data-part="add-host-submit">
            Add host
          </Button>
        </>
      }
    >
      <div class="nc-stack">
      <TextField
        label="Name"
        help="Lowercase letters, digits and hyphens. Commands and grants name the host by it."
        value={name()}
        placeholder="studio"
        onInput={event => setName(event.currentTarget.value)}
      />
      <TextField
        label="Address"
        help="Where Rookery reaches the machine before it joins a tailnet."
        value={address()}
        placeholder="192.0.2.10"
        onInput={event => setAddress(event.currentTarget.value)}
      />
      <Select
        label="Type"
        value={type()}
        options={[
          { value: String(HostType.SSH), label: "SSH — reached over SSH only, until it is applied" },
          { value: String(HostType.MANAGED), label: "Managed — rookeryd, box and the tailnet" },
          { value: String(HostType.WORKSTATION), label: "Workstation — a managed host with a desktop" },
        ]}
        onChange={setType}
      />
      <div class="nc-row">
        <TextField label="SSH user" value={user()} onInput={event => setUser(event.currentTarget.value)} />
        <TextField label="Port" value={port()} onInput={event => setPort(event.currentTarget.value)} />
      </div>
      <Select
        label="First contact"
        value={kind()}
        options={[
          { value: "none", label: "No credential — an apply or a later check contacts it" },
          { value: "password", label: "A password, used once" },
          { value: "key", label: "A private key, used once" },
        ]}
        onChange={value => setKind(value as CredentialKind)}
      />
      <Show when={kind() === "password"}>
        <TextField
          label="Password"
          type="password"
          help="Sent once for first contact. The server does not store it."
          value={password()}
          onInput={event => setPassword(event.currentTarget.value)}
        />
      </Show>
      <Show when={kind() === "key"}>
        <TextArea
          label="Private key"
          help="PEM, unencrypted. Sent once for first contact. The server does not store it."
          rows={5}
          value={key()}
          onInput={event => setKey(event.currentTarget.value)}
        />
      </Show>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      </div>
    </Dialog>
  );
}
