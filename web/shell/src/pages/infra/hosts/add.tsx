// "Add host": a machine Rookery reaches over SSH. A password or a private key
// given here is used once, by first contact, to install the server's own key; it
// is never stored.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { HostType, type Host } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Button, Callout, Field, Input, Select, Spinner } from "../../../ui.tsx";
import { errorMessage } from "../format.ts";
import { Dialog, TextArea } from "../parts.tsx";

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
          <Button variant="primary" onClick={() => void submit()} disabled={busy()} data-part="add-host-submit">
            {busy() ? "Adding…" : "Add host"}
          </Button>
        </>
      }
    >
      <Field label="Name" hint="Lowercase letters, digits and hyphens. Commands and grants name the host by it.">
        <Input value={name()} placeholder="studio" onInput={setName} />
      </Field>
      <Field label="Address" hint="Where Rookery reaches the machine before it joins a tailnet.">
        <Input value={address()} placeholder="192.0.2.10" onInput={setAddress} />
      </Field>
      <Field label="Type">
        <Select
          value={type()}
          options={[
            { value: String(HostType.SSH), label: "SSH — reached over SSH only, until it is applied" },
            { value: String(HostType.MANAGED), label: "Managed — rookeryd, box and the tailnet" },
            { value: String(HostType.WORKSTATION), label: "Workstation — a managed host with a desktop" },
          ]}
          onChange={setType}
        />
      </Field>
      <div class="nc-row">
        <Field label="SSH user">
          <Input value={user()} onInput={setUser} />
        </Field>
        <Field label="Port">
          <Input value={port()} onInput={setPort} />
        </Field>
      </div>
      <Field label="First contact">
        <Select
          value={kind()}
          options={[
            { value: "none", label: "No credential — an apply or a later check contacts it" },
            { value: "password", label: "A password, used once" },
            { value: "key", label: "A private key, used once" },
          ]}
          onChange={value => setKind(value as CredentialKind)}
        />
      </Field>
      <Show when={kind() === "password"}>
        <Field label="Password" hint="Sent once for first contact. The server does not store it.">
          <Input type="password" value={password()} onInput={setPassword} />
        </Field>
      </Show>
      <Show when={kind() === "key"}>
        <TextArea
          label="Private key"
          value={key()}
          rows={5}
          onInput={setKey}
        />
        <p class="nc-muted">PEM, unencrypted. Sent once for first contact. The server does not store it.</p>
      </Show>
      <Show when={failure()}>
        {message => <Callout tone="danger">{message()}</Callout>}
      </Show>
      <Show when={busy()}>
        <span class="nc-row">
          <Spinner /> Adding the host…
        </span>
      </Show>
    </Dialog>
  );
}
