// Plugins and registries: what is installed, what it adds, and where more comes
// from.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Plugin } from "@rookery/proto";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Code, Empty, Field, Input, Page, Spinner, Switch, Table } from "../../ui.tsx";

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
        actions={
          <Button size="sm" variant="primary" onClick={() => void installFromRegistry()}>
            Install from a registry
          </Button>
        }
      >
        <div class="nc-stack">
          <div class="nc-row">
            <Field label="Reference" hint="A plugin's id, optionally with a version: example or example@0.2.0.">
              <Input id="plugin-reference" value={reference()} placeholder="example@0.2.0" onInput={setReference} />
            </Field>
          </div>
          <div class="nc-row">
            <Field label="Package" hint="A .tar.gz with plugin.yaml at its root.">
              <input
                type="file"
                accept=".tar.gz,application/gzip"
                onChange={(event) => {
                  const file = event.currentTarget.files?.[0];
                  if (file !== undefined) void installFromPackage(file);
                }}
              />
            </Field>
          </div>
        </div>
      </Card>

      <Card
        title="Installed"
        actions={
          <Switch label="Remove with data" checked={purge()} onChange={setPurge} />
        }
      >
        <Show when={plugins() !== undefined} fallback={<Spinner />}>
          <Show
            when={(plugins()?.plugins.length ?? 0) > 0}
            fallback={<Empty title="No plugins" description="The release installs Docker and Incus on an empty database." />}
          >
            <Table columns={["Plugin", "Version", "State", "Adds", ""]}>
              <For each={plugins()?.plugins ?? []}>
                {plugin => (
                  <tr>
                    <td>
                      <strong>{plugin.name || plugin.id}</strong>
                      <span class="nc-meta"> {plugin.id}</span>
                    </td>
                    <td class="nc-mono">{plugin.version}</td>
                    <td>
                      <Show when={plugin.enabled} fallback={<Badge>disabled</Badge>}>
                        <Badge tone="success">enabled</Badge>
                      </Show>
                      <Show when={plugin.shell}>
                        <Badge tone="accent">shell</Badge>
                      </Show>
                    </td>
                    <td>{contributionSummary(plugin)}</td>
                    <td>
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
                            void run(
                              () => shell.rookery.plugins.remove({ id: plugin.id, purge: purge() }),
                              `${plugin.id} is removed.`,
                            )
                          }
                        >
                          Remove
                        </Button>
                      </div>
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>

      <Card
        title="Registries"
        actions={
          <div class="nc-row nc-row--tight">
            <Input id="registry-name" value={registryName()} placeholder="name" onInput={setRegistryName} />
            <Input id="registry-url" type="url" value={registryURL()} placeholder="https://plugins.example.test" onInput={setRegistryURL} />
            <Input id="registry-key" value={registryKey()} placeholder="public key" onInput={setRegistryKey} />
            <Button size="sm" onClick={() => void addRegistry()}>
              Add
            </Button>
          </div>
        }
      >
        <Show when={registries() !== undefined} fallback={<Spinner />}>
          <Show
            when={(registries()?.registries.length ?? 0) > 0}
            fallback={<Empty title="No registries" description="A registry is a signed index of packages, over HTTPS." />}
          >
            <Table columns={["Name", "URL", "Key", ""]}>
              <For each={registries()?.registries ?? []}>
                {registry => (
                  <tr>
                    <td>{registry.name}</td>
                    <td class="nc-mono">{registry.url}</td>
                    <td>
                      <Code>{new TextDecoder().decode(registry.publicKey).slice(0, 32)}…</Code>
                    </td>
                    <td>
                      <Button
                        size="sm"
                        variant="danger"
                        onClick={() => {
                          void shell.rookery.registries.remove({ name: registry.name }).then(() => registries.refresh());
                        }}
                      >
                        Remove
                      </Button>
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
