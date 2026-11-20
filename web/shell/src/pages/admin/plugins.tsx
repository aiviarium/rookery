// Plugins and registries: what is installed, what it adds, and where more comes
// from.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Plugin, Registry } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  DataTable,
  type DataColumn,
  Switch,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { FileField } from "../../parts/file-field.tsx";
import { Page } from "../../parts/page.tsx";

/** PluginsPage installs, updates, enables and removes plugins. */
export function PluginsPage(): JSX.Element {
  const shell = useShell();
  const plugins = createQuery(() => shell.rookery.plugins.list({}));
  const registries = createQuery(() => shell.rookery.registries.list({}));

  const [reference, setReference] = createSignal("");
  const [error, setError] = createSignal("");
  const [message, setMessage] = createSignal("");
  const [purge, setPurge] = createSignal(false);

  const [registryName, setRegistryName] = createSignal("");
  const [registryURL, setRegistryURL] = createSignal("");
  const [registryKey, setRegistryKey] = createSignal("");

  const run = async (work: () => Promise<unknown>, done: string) => {
    setError("");
    setMessage("");
    try {
      await work();
      setMessage(done);
      void plugins.refresh();
      void shell.plugins.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const installFromRegistry = () =>
    run(() => shell.rookery.plugins.installFromRegistry({ reference: reference().trim() }), "Installed.");

  const installFromPackage = async (file: File) => {
    const bytes = new Uint8Array(await file.arrayBuffer());
    await run(() => shell.rookery.plugins.installPackage({ package: bytes }), "Installed.");
  };

  const addRegistry = async () => {
    setError("");
    try {
      await shell.rookery.registries.add({
        registry: {
          name: registryName().trim(),
          url: registryURL().trim(),
          publicKey: new TextEncoder().encode(registryKey()),
        },
      });
      setRegistryName("");
      setRegistryURL("");
      setRegistryKey("");
      void registries.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const pluginColumns = (): DataColumn<Plugin>[] => [
    {
      id: "plugin",
      header: "Plugin",
      accessor: plugin => plugin.name || plugin.id,
      cell: plugin => (
        <span class="nc-row nc-row--tight">
          <strong>{plugin.name || plugin.id}</strong>
          <span class="nc-meta">{plugin.id}</span>
        </span>
      ),
    },
    { id: "version", header: "Version", type: "mono", accessor: plugin => plugin.version },
    {
      id: "state",
      header: "State",
      accessor: plugin => (plugin.enabled ? "enabled" : "disabled"),
      cell: plugin => (
        <span class="nc-row nc-row--tight">
          <Show when={plugin.enabled} fallback={<Badge>disabled</Badge>}>
            <Badge tone="success">enabled</Badge>
          </Show>
          <Show when={plugin.shell}>
            <Badge tone="accent">shell</Badge>
          </Show>
        </span>
      ),
    },
    { id: "adds", header: "Adds", accessor: plugin => contributionSummary(plugin) },
    {
      id: "actions",
      header: "",
      cell: plugin => (
        <div class="nc-row nc-row--tight">
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              void run(
                () => shell.rookery.plugins.setEnabled({ id: plugin.id, enabled: !plugin.enabled }),
                plugin.enabled ? `${plugin.id} is disabled.` : `${plugin.id} is enabled.`,
              )
            }
          >
            {plugin.enabled ? "Disable" : "Enable"}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              void run(
                () => shell.rookery.plugins.update({ reference: `${plugin.id}@${plugin.version}` }),
                `${plugin.id} is updated.`,
              )
            }
          >
            Update
          </Button>
          <Button
            size="sm"
            variant="danger"
            onClick={() =>
              void run(() => shell.rookery.plugins.remove({ id: plugin.id, purge: purge() }), `${plugin.id} is removed.`)
            }
          >
            Remove
          </Button>
        </div>
      ),
    },
  ];

  const registryColumns = (): DataColumn<Registry>[] => [
    { id: "name", header: "Name", accessor: registry => registry.name },
    { id: "url", header: "URL", type: "mono", accessor: registry => registry.url },
    {
      id: "key",
      header: "Key",
      cell: registry => (
        <code class="nc-code">{new TextDecoder().decode(registry.publicKey).slice(0, 32)}…</code>
      ),
    },
    {
      id: "actions",
      header: "",
      cell: registry => (
        <Button
          size="sm"
          variant="danger"
          onClick={() => {
            void shell.rookery.registries.remove({ name: registry.name }).then(() => registries.refresh());
          }}
        >
          Remove
        </Button>
      ),
    },
  ];

  return (
    <Page title="Plugins" description="What this deployment runs, and where more comes from.">
      <Show when={message() !== ""}>
        <Callout tone="info">{message()}</Callout>
      </Show>
      <Show when={error() !== ""}>
        <Callout tone="danger">{error()}</Callout>
      </Show>

      <Card
        title="Install"
        action={
          <Button size="sm" variant="primary" onClick={() => void installFromRegistry()}>
            Install from a registry
          </Button>
        }
      >
        <div class="nc-stack">
          <div class="nc-row">
            <TextField
              label="Reference"
              help="A plugin's id, optionally with a version: example or example@0.2.0."
              id="plugin-reference"
              value={reference()}
              placeholder="example@0.2.0"
              onInput={event => setReference(event.currentTarget.value)}
            />
          </div>
          <div class="nc-row">
            <FileField
              label="Package"
              help="A .tar.gz with plugin.yaml at its root."
              accept=".tar.gz,application/gzip"
              onPick={file => void installFromPackage(file)}
            />
          </div>
        </div>
      </Card>

      <Card
        title="Installed"
        action={<Switch label="Remove with data" checked={purge()} onChange={setPurge} />}
        padding="none"
      >
        <DataTable
          bare
          label="Installed plugins"
          rows={plugins()?.plugins ?? []}
          columns={pluginColumns()}
          getRowId={plugin => plugin.id}
          loading={plugins.pending() && plugins() === undefined}
          empty={{ title: "No plugins", description: "The release installs Docker and Incus on an empty database." }}
        />
      </Card>

      <Card
        title="Registries"
        action={
          <div class="nc-row nc-row--tight">
            <TextField
              aria-label="The registry's name"
              id="registry-name"
              value={registryName()}
              placeholder="name"
              onInput={event => setRegistryName(event.currentTarget.value)}
            />
            <TextField
              aria-label="The registry's URL"
              id="registry-url"
              type="url"
              value={registryURL()}
              placeholder="https://plugins.example.test"
              onInput={event => setRegistryURL(event.currentTarget.value)}
            />
            <TextField
              aria-label="The registry's public key"
              id="registry-key"
              value={registryKey()}
              placeholder="public key"
              onInput={event => setRegistryKey(event.currentTarget.value)}
            />
            <Button size="sm" onClick={() => void addRegistry()}>
              Add
            </Button>
          </div>
        }
        padding="none"
      >
        <DataTable
          bare
          label="Registries"
          rows={registries()?.registries ?? []}
          columns={registryColumns()}
          getRowId={registry => registry.name}
          loading={registries.pending() && registries() === undefined}
          empty={{ title: "No registries", description: "A registry is a signed index of packages, over HTTPS." }}
        />
      </Card>
    </Page>
  );
}

/** contributionSummary is what a plugin adds, in a few words. */
function contributionSummary(plugin: Plugin): string {
  if (plugin.contributesJson.length === 0) return "—";
  try {
    const contributes = JSON.parse(new TextDecoder().decode(plugin.contributesJson)) as {
      views?: unknown[];
      hostTabs?: unknown[];
      settings?: unknown;
    };
    const parts: string[] = [];
    if (contributes.views !== undefined) parts.push(`${contributes.views.length} views`);
    if (contributes.hostTabs !== undefined) parts.push(`${contributes.hostTabs.length} host tabs`);
    return parts.length > 0 ? parts.join(", ") : "—";
  } catch {
    return "—";
  }
}
