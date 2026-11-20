// The tailnet: the network the server and its hosts meet on.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Empty, Field, Input, Page, Spinner, Table } from "../../ui.tsx";

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

  return (
    <Page title="Tailnet" description="The network the server and its hosts are devices of.">
      <Card title="Status">
        <Show when={status() !== undefined} fallback={<Spinner />}>
          <div class="nc-stack">
            <div class="nc-row" style={{ gap: "var(--space-3)" }}>
              <span class="nc-row nc-row--tight">
                <Show when={status()?.configured} fallback={<Badge>not configured</Badge>}>
                  <Badge tone="success">configured</Badge>
                </Show>
              </span>
              <Show when={status()?.configured}>
                <Badge tone={status()?.online ? "success" : "warning"}>{status()?.online ? "online" : "offline"}</Badge>
              </Show>
              <Show when={status()?.controlUrl !== ""}>
                <span class="nc-meta">control: {status()?.controlUrl}</span>
              </Show>
              <Show when={status()?.tag !== ""}>
                <span class="nc-meta">tag: {status()?.tag}</span>
              </Show>
              <For each={status()?.addresses ?? []}>{(address) => <span class="nc-mono">{address}</span>}</For>
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
            <Field label="Control URL" hint="Tailscale's hosted service, or the Headscale that serves this deployment.">
              <Input id="tailnet-url" type="url" value={controlURL()} onInput={setControlURL} />
            </Field>
            <Field label="API key">
              <Input id="tailnet-api-key" type="password" autocomplete="new-password" value={apiKey()} onInput={setAPIKey} />
            </Field>
            <Field label="Tag">
              <Input id="tailnet-tag" value={tag()} onInput={setTag} />
            </Field>
            <Button variant="primary" size="sm" onClick={() => void configure()}>
              Save
            </Button>
          </div>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
        </div>
      </Card>

      <Card title="Devices">
        <Show when={devices() !== undefined} fallback={<Spinner />}>
          <Show
            when={(devices()?.devices.length ?? 0) > 0}
            fallback={<Empty title="No devices" description="Each host that joins the tailnet is a device here." />}
          >
            <Table columns={["Hostname", "Addresses", "Tags", "State"]}>
              <For each={devices()?.devices ?? []}>
                {device => (
                  <tr>
                    <td>{device.hostname}</td>
                    <td class="nc-mono">{device.addresses.join(", ")}</td>
                    <td>{device.tags.join(", ")}</td>
                    <td>
                      <Show when={device.online} fallback={<Badge>offline</Badge>}>
                        <Badge tone="success">online</Badge>
                      </Show>
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>
    </Page>
  );
}
