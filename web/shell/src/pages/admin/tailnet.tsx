// The tailnet: the network the server and its hosts meet on.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { TailnetDevice } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  DataTable,
  type DataColumn,
  MetaLine,
  Skeleton,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

/** TailnetPage is the tailnet's state, its settings and its devices. */
export function TailnetPage(): JSX.Element {
  const shell = useShell();
  const status = createQuery(() => shell.rookery.tailnet.status({}));
  const devices = createQuery(() => shell.rookery.tailnet.listDevices({}));

  const [controlURL, setControlURL] = createSignal("https://controlplane.tailscale.com");
  const [apiKey, setAPIKey] = createSignal("");
  const [tag, setTag] = createSignal("tag:rookery");
  const [error, setError] = createSignal("");
  const [configured, setConfigured] = createSignal(false);

  const configure = async () => {
    setError("");
    try {
      await shell.rookery.tailnet.configure({ controlUrl: controlURL().trim(), apiKey: apiKey(), tag: tag().trim() });
      setAPIKey("");
      setConfigured(true);
      void status.refresh();
      void devices.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const columns = (): DataColumn<TailnetDevice>[] => [
    { id: "hostname", header: "Hostname", accessor: device => device.hostname },
    { id: "addresses", header: "Addresses", type: "mono", accessor: device => device.addresses.join(", ") },
    { id: "tags", header: "Tags", accessor: device => device.tags.join(", ") },
    {
      id: "state",
      header: "State",
      accessor: device => (device.online ? "online" : "offline"),
      cell: device => (
        <Show when={device.online} fallback={<Badge>offline</Badge>}>
          <Badge tone="success">online</Badge>
        </Show>
      ),
    },
  ];

  return (
    <Page title="Tailnet" description="The network the server and its hosts are devices of.">
      <Card title="Status">
        <Show when={status() !== undefined} fallback={<Skeleton width="30%" />}>
          <div class="nc-stack">
            <div class="nc-row" style={{ gap: "var(--space-3)" }}>
              <Show when={status()?.configured} fallback={<Badge>not configured</Badge>}>
                <Badge tone="success">configured</Badge>
              </Show>
              <Show when={status()?.configured}>
                <Badge tone={status()?.online ? "success" : "warning"}>{status()?.online ? "online" : "offline"}</Badge>
              </Show>
              <MetaLine
                items={[
                  status()?.controlUrl !== "" ? { label: `control: ${status()?.controlUrl}`, mono: true } : false,
                  status()?.tag !== "" ? { label: `tag: ${status()?.tag}`, mono: true } : false,
                  ...(status()?.addresses ?? []).map(address => ({ label: address, mono: true })),
                ]}
              />
            </div>
            <Show when={configured()}>
              <Callout tone="info">The tailnet is configured. A host joins it when it is applied.</Callout>
            </Show>
          </div>
        </Show>
      </Card>

      <Card title="Configure">
        <div class="nc-stack">
          <div class="nc-row">
            <TextField
              label="Control URL"
              help="Tailscale's hosted service, or the Headscale that serves this deployment."
              id="tailnet-url"
              type="url"
              value={controlURL()}
              onInput={event => setControlURL(event.currentTarget.value)}
            />
            <TextField
              label="API key"
              id="tailnet-api-key"
              type="password"
              autocomplete="new-password"
              value={apiKey()}
              onInput={event => setAPIKey(event.currentTarget.value)}
            />
            <TextField label="Tag" id="tailnet-tag" value={tag()} onInput={event => setTag(event.currentTarget.value)} />
            <Button variant="primary" size="sm" onClick={() => void configure()}>
              Save
            </Button>
          </div>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
        </div>
      </Card>

      <Card title="Devices" count={devices()?.devices.length} padding="none">
        <DataTable
          bare
          label="Devices"
          rows={devices()?.devices ?? []}
          columns={columns()}
          getRowId={device => device.id}
          loading={devices.pending() && devices() === undefined}
          empty={{ title: "No devices", description: "Each host that joins the tailnet is a device here." }}
        />
      </Card>
    </Page>
  );
}
