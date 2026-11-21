// The tailnet: the network the server and its hosts meet on. The server joins
// it once it is configured, and each host joins when it is applied.
import { For, Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { TailnetDevice, TailnetServiceStatusResponse } from "@rookery/proto";
import { Badge, Button, Card, DataTable, type DataColumn, EmptyState, PropertyList, Skeleton, State, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";

/** HOSTED is Tailscale's own control server. */
const HOSTED = "https://controlplane.tailscale.com";

/** TailnetPage is the tailnet's state, its settings and its devices. */
export function TailnetPage(): JSX.Element {
  const shell = useShell();
  const status = createLive(() => shell.rookery.tailnet.status({}));
  const devices = createLive(() => shell.rookery.tailnet.listDevices({}));
  const [configuring, setConfiguring] = createSignal(false);
  const refresh = () => {
    void status.refresh().catch(() => undefined);
    void devices.refresh().catch(() => undefined);
  };

  const columns = (): DataColumn<TailnetDevice>[] => [
    { id: "hostname", header: "Hostname", accessor: (device) => device.hostname },
    { id: "addresses", header: "Addresses", type: "mono", accessor: (device) => device.addresses.join(", ") },
    {
      id: "tags",
      header: "Tags",
      accessor: (device) => device.tags.join(", "),
      cell: (device) => (
        <span class="nc-row nc-row--tight">
          <For each={device.tags}>{(tag) => <Badge tone="outline">{tag}</Badge>}</For>
        </span>
      ),
    },
    {
      id: "state",
      header: "State",
      accessor: (device) => (device.online ? "online" : "offline"),
      cell: (device) => <State status={device.online ? "ok" : "off"}>{device.online ? "Online" : "Offline"}</State>,
    },
  ];

  const configured = () => status()?.configured === true;
  const failure = () => status.error() ?? devices.error();

  return (
    <View
      id="tailnet"
      crumbs={[{ label: "Tailnet" }]}
      context={
        <Show when={status()}>
          {(now) => (
            <State status={!now().configured ? "off" : now().online ? "ok" : "warn"}>
              {!now().configured ? "Not configured" : now().online ? "Online" : "Offline"}
            </State>
          )}
        </Show>
      }
      error={failure() === undefined ? undefined : `The tailnet could not be read. ${errorMessage(failure())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: refresh },
        {
          id: "configure",
          label: configured() ? "Change settings" : "Configure",
          icon: "settings",
          primary: true,
          onClick: () => setConfiguring(true),
        },
      ]}
    >
      <Show when={status()} fallback={<Skeleton width="40%" />}>
        {(now) => (
          <Show
            when={now().configured}
            fallback={
              <Card>
                <EmptyState
                  icon="link"
                  title="No tailnet yet"
                  description="With a tailnet, the server reaches its hosts at private addresses, and a host needs no open SSH port. Tailscale and Headscale both work."
                >
                  <Button variant="primary" icon="settings" onClick={() => setConfiguring(true)}>
                    Configure
                  </Button>
                </EmptyState>
              </Card>
            }
          >
            <Card title="This server">
              <PropertyList
                items={[
                  { label: "Control", value: <span class="nc-mono">{now().controlUrl}</span> },
                  { label: "Tag", value: <span class="nc-mono">{now().tag}</span> },
                  {
                    label: "Addresses",
                    value: now().addresses.length === 0 ? "None yet" : <span class="nc-mono">{now().addresses.join(", ")}</span>,
                  },
                ]}
              />
            </Card>
            <Card title="Devices" count={devices()?.devices.length} padding="none">
              <DataTable
                bare
                label="Devices"
                rows={devices()?.devices ?? []}
                columns={columns()}
                getRowId={(device) => device.id}
                loading={devices() === undefined}
                empty={{ title: "No devices", description: "Each host that joins the tailnet is a device here." }}
              />
            </Card>
          </Show>
        )}
      </Show>
      <Show when={configuring()}>
        <ConfigureDialog status={status()} onClose={() => setConfiguring(false)} onDone={refresh} />
      </Show>
    </View>
  );
}

function ConfigureDialog(props: {
  status: TailnetServiceStatusResponse | undefined;
  onClose: () => void;
  onDone: () => void;
}): JSX.Element {
  const shell = useShell();
  const initial = untrack(() => props.status);
  const [controlUrl, setControlUrl] = createSignal(initial?.controlUrl || HOSTED);
  const [apiKey, setApiKey] = createSignal("");
  const [tag, setTag] = createSignal(initial?.tag || "tag:rookery");
  const save = createAction(async () => {
    await shell.rookery.tailnet.configure({ controlUrl: controlUrl().trim(), apiKey: apiKey(), tag: tag().trim() });
  }, { done: "Tailnet configured" });
  return (
    <FormDialog
      open
      title="Tailnet settings"
      description="Hosts join on their next apply."
      submit="Save"
      action={save}
      disabled={controlUrl().trim() === "" || apiKey() === "" || tag().trim() === ""}
      onClose={props.onClose}
      onDone={props.onDone}
    >
      <div class="nc-stack">
        <TextField
          label="Control URL"
          type="url"
          mono
          help="Tailscale's hosted service, or your Headscale."
          value={controlUrl()}
          onInput={(event) => setControlUrl(event.currentTarget.value)}
        />
        <TextField
          label="API key"
          type="password"
          autocomplete="new-password"
          help="The server keeps it encrypted, and it is never shown again."
          value={apiKey()}
          onInput={(event) => setApiKey(event.currentTarget.value)}
        />
        <TextField label="Tag" mono help="Every device Rookery adds carries it." value={tag()} onInput={(event) => setTag(event.currentTarget.value)} />
      </div>
    </FormDialog>
  );
}
