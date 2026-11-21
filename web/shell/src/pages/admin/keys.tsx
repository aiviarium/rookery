// Keys: the ring the server logs in to its hosts with, and the host keys it
// trusts. A host whose key changed waits for an admin to trust the new one.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Key, KnownHost } from "@rookery/proto";
import { Badge, Callout, Card, DataTable, type DataColumn, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";

/** KeysPage is the key ring and the known hosts. */
export function KeysPage(): JSX.Element {
  const shell = useShell();
  const keys = createLive(() => shell.rookery.keys.list({}));
  const known = createLive(() => shell.rookery.keys.listKnownHosts({}));
  const [rotating, setRotating] = createSignal(false);
  const [forgetting, setForgetting] = createSignal<KnownHost>();
  const refresh = () => {
    void keys.refresh().catch(() => undefined);
    void known.refresh().catch(() => undefined);
  };

  const trust = createAction(async (host: KnownHost) => {
    await shell.rookery.keys.trustHostKey({ address: host.address, fingerprint: host.pendingFingerprint });
    void known.refresh().catch(() => undefined);
  }, { done: "New host key trusted", failed: "The key was not trusted" });
  const forget = createAction(async () => {
    const host = forgetting();
    if (host !== undefined) await shell.rookery.keys.forgetHostKey({ address: host.address });
    void known.refresh().catch(() => undefined);
  }, { done: "Host key forgotten" });

  const changed = () => (known()?.hosts ?? []).filter((host) => host.pendingFingerprint !== "");

  const keyColumns = (): DataColumn<Key>[] => [
    { id: "name", header: "Name", accessor: (key) => key.name },
    { id: "type", header: "Type", accessor: (key) => key.type },
    { id: "fingerprint", header: "Fingerprint", type: "mono", accessor: (key) => key.fingerprint },
    {
      id: "state",
      header: "State",
      accessor: (key) => (key.active ? "active" : "retired"),
      cell: (key) => (
        <Show when={key.active} fallback={<span class="nc-meta">retired</span>}>
          <Badge tone="success">active</Badge>
        </Show>
      ),
    },
  ];

  const knownColumns = (): DataColumn<KnownHost>[] => [
    { id: "address", header: "Address", type: "mono", accessor: (host) => host.address },
    { id: "host", header: "Host", accessor: (host) => host.host },
    {
      id: "fingerprint",
      header: "Fingerprint",
      type: "mono",
      accessor: (host) => (host.pendingFingerprint !== "" ? host.pendingFingerprint : host.fingerprint),
    },
    {
      id: "trust",
      header: "Trust",
      accessor: (host) => (host.pendingFingerprint !== "" ? "changed" : "trusted"),
      cell: (host) => (
        <Show when={host.pendingFingerprint !== ""} fallback={<Badge tone="success">trusted</Badge>}>
          <Badge tone="warning">changed</Badge>
        </Show>
      ),
    },
    {
      id: "actions",
      header: "",
      cell: (host) => (
        <RowActions
          label={`Actions for ${host.address}`}
          items={[
            ...(host.pendingFingerprint !== "" ? [{ id: "trust", label: "Trust the new key", icon: "check" as const }] : []),
            { id: "forget", label: "Forget", icon: "trash" as const, danger: true },
          ]}
          onSelect={(id) => (id === "trust" ? void trust.run(host) : setForgetting(host))}
        />
      ),
    },
  ];

  const failure = () => keys.error() ?? known.error();

  return (
    <View
      id="keys"
      crumbs={[{ label: "Keys" }]}
      error={failure() === undefined ? undefined : `The keys could not be read. ${errorMessage(failure())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: refresh },
        { id: "rotate", label: "Rotate key", icon: "rotate-ccw", primary: true, onClick: () => setRotating(true) },
      ]}
    >
      <Show when={changed().length > 0}>
        <Callout tone="warning">
          {changed().length === 1
            ? `The key of ${changed()[0]!.address} changed. Rookery does not reach it until an admin trusts the new key.`
            : `${changed().length} hosts changed their keys. Rookery does not reach them until an admin trusts the new keys.`}
        </Callout>
      </Show>
      <Card title="Key ring" count={keys()?.keys.length} padding="none">
        <DataTable
          bare
          label="Key ring"
          rows={keys()?.keys ?? []}
          columns={keyColumns()}
          getRowId={(key) => key.id}
          loading={keys() === undefined}
          empty={{ title: "No keys", description: "The server makes its ring on its first start." }}
        />
      </Card>
      <Card title="Known hosts" count={known()?.hosts.length} padding="none">
        <DataTable
          bare
          label="Known hosts"
          rows={known()?.hosts ?? []}
          columns={knownColumns()}
          getRowId={(host) => host.address}
          loading={known() === undefined}
          empty={{
            title: "No host keys yet",
            description: "The first contact with a host records its key. A changed key waits for an admin.",
          }}
        />
      </Card>

      <Show when={rotating()}>
        <RotateDialog onClose={() => setRotating(false)} onRotated={() => void keys.refresh().catch(() => undefined)} />
      </Show>
      <ConfirmDialog
        open={forgetting() !== undefined}
        title={`Forget ${forgetting()?.address ?? "the host key"}?`}
        description="The next contact records whatever key the host offers."
        confirm="Forget key"
        danger
        action={forget}
        onClose={() => setForgetting(undefined)}
      />
    </View>
  );
}

function RotateDialog(props: { onClose: () => void; onRotated: () => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const rotate = createAction(async () => {
    await shell.rookery.keys.rotate({ name: name().trim() });
  }, { done: "Key rotated" });
  return (
    <FormDialog
      open
      title="Rotate the server's key"
      description="The ring makes a new key and uses it from now on. Hosts take it on their next apply. The old key stays until then."
      submit="Rotate"
      action={rotate}
      onClose={props.onClose}
      onDone={props.onRotated}
    >
      <TextField
        label="Name"
        placeholder="default"
        help="Leave it empty for the default name."
        value={name()}
        onInput={(event) => setName(event.currentTarget.value)}
      />
    </FormDialog>
  );
}
