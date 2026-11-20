// Keys: the ring the server logs in to its hosts with, and the host keys it
// trusts.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Key, KnownHost } from "@rookery/proto";
import { Badge, Button, Callout, Card, DataTable, type DataColumn, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

/** KeysPage is the key ring and the known hosts. */
export function KeysPage(): JSX.Element {
  const shell = useShell();
  const keys = createQuery(() => shell.rookery.keys.list({}));
  const known = createQuery(() => shell.rookery.keys.listKnownHosts({}));

  const [name, setName] = createSignal("");
  const [error, setError] = createSignal("");
  const [message, setMessage] = createSignal("");

  const rotate = async () => {
    setError("");
    setMessage("");
    try {
      const answer = await shell.rookery.keys.rotate({ name: name().trim() });
      setMessage(`The ring now uses ${answer.key?.fingerprint ?? "a new key"}.`);
      setName("");
      void keys.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const keyColumns = (): DataColumn<Key>[] => [
    { id: "name", header: "Name", accessor: key => key.name },
    { id: "type", header: "Type", accessor: key => key.type },
    { id: "fingerprint", header: "Fingerprint", type: "mono", accessor: key => key.fingerprint },
    {
      id: "in-use",
      header: "In use",
      accessor: key => (key.active ? "active" : "retired"),
      cell: key => (
        <Show when={key.active} fallback={<span class="nc-meta">retired</span>}>
          <Badge tone="success">active</Badge>
        </Show>
      ),
    },
  ];

  const knownColumns = (): DataColumn<KnownHost>[] => [
    { id: "address", header: "Address", type: "mono", accessor: host => host.address },
    { id: "host", header: "Host", accessor: host => host.host },
    {
      id: "fingerprint",
      header: "Fingerprint",
      type: "mono",
      accessor: host => (host.pendingFingerprint !== "" ? host.pendingFingerprint : host.fingerprint),
    },
    {
      id: "trust",
      header: "Trust",
      accessor: host => (host.pendingFingerprint !== "" ? "changed" : "trusted"),
      cell: host => (
        <Show when={host.pendingFingerprint !== ""} fallback={<Badge tone="success">trusted</Badge>}>
          <Badge tone="warning">changed</Badge>
        </Show>
      ),
    },
    {
      id: "actions",
      header: "",
      cell: host => (
        <div class="nc-row nc-row--tight">
          <Show when={host.pendingFingerprint !== ""}>
            <Button
              size="sm"
              variant="primary"
              onClick={() => {
                void shell.rookery.keys
                  .trustHostKey({ address: host.address, fingerprint: host.pendingFingerprint })
                  .then(() => known.refresh());
              }}
            >
              Trust the new key
            </Button>
          </Show>
          <Button
            size="sm"
            variant="danger"
            onClick={() => {
              void shell.rookery.keys.forgetHostKey({ address: host.address }).then(() => known.refresh());
            }}
          >
            Forget
          </Button>
        </div>
      ),
    },
  ];

  return (
    <Page title="Keys" description="The SSH keys the server uses, and the host keys it trusts.">
      <Card
        title="Key ring"
        action={
          <div class="nc-row nc-row--tight">
            <TextField
              aria-label="The new key's name"
              id="key-name"
              value={name()}
              placeholder="Name (default)"
              onInput={event => setName(event.currentTarget.value)}
            />
            <Button size="sm" variant="primary" onClick={() => void rotate()}>
              Rotate
            </Button>
          </div>
        }
        padding="none"
      >
        <div class="shell-pad nc-stack">
          <Show when={message() !== ""}>
            <Callout tone="info">{message()}</Callout>
          </Show>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
        </div>
        <DataTable
          bare
          label="Key ring"
          rows={keys()?.keys ?? []}
          columns={keyColumns()}
          getRowId={key => key.id}
          loading={keys.pending() && keys() === undefined}
          empty={{ title: "No keys", description: "The ring is made on the server's first start." }}
        />
      </Card>

      <Card title="Known hosts" count={known()?.hosts.length} padding="none">
        <DataTable
          bare
          label="Known hosts"
          rows={known()?.hosts ?? []}
          columns={knownColumns()}
          getRowId={host => host.address}
          loading={known.pending() && known() === undefined}
          empty={{
            title: "No host keys yet",
            description: "First contact records a host's key, and a changed key needs an admin's yes.",
          }}
        />
      </Card>
    </Page>
  );
}
