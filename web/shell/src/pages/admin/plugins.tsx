// Plugins and the registries they come from. Installing, updating or turning a
// plugin on or off reloads the client halves the shell hosts.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Plugin, Registry } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  EmptyState,
  ListRow,
  RowList,
  Skeleton,
  Switch,
  TabPanel,
  Tabs,
  TextArea,
  TextField,
} from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { FileField } from "../../parts/file-field.tsx";
import { RowActions } from "../../parts/row-actions.tsx";
import { createTab } from "../../parts/tab.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { dateOf, relativeTime } from "../infra/format.ts";

const TABS = ["installed", "registries"] as const;
type Tab = (typeof TABS)[number];

/** PluginsPage installs, updates, enables and removes plugins, and keeps the registries. */
export function PluginsPage(): JSX.Element {
  const shell = useShell();
  const plugins = createLive(() => shell.rookery.plugins.list({}));
  const registries = createLive(() => shell.rookery.registries.list({}));
  const [tab, setTab] = createTab<Tab>(TABS);
  const [installing, setInstalling] = createSignal(false);
  const [addingRegistry, setAddingRegistry] = createSignal(false);
  const [removing, setRemoving] = createSignal<Plugin>();
  const [removingRegistry, setRemovingRegistry] = createSignal<Registry>();

  const changed = () => {
    void plugins.refresh().catch(() => undefined);
    void shell.plugins.reload();
  };
  const refreshRegistries = () => void registries.refresh().catch(() => undefined);

  const change = createAction(async (plugin: Plugin, id: string) => {
    if (id === "toggle") await shell.rookery.plugins.setEnabled({ id: plugin.id, enabled: !plugin.enabled });
    if (id === "update") await shell.rookery.plugins.update({ reference: plugin.id });
    changed();
  }, { failed: "The plugin did not change" });
  const removeRegistry = createAction(async () => {
    const registry = removingRegistry();
    if (registry !== undefined) await shell.rookery.registries.remove({ name: registry.name });
    refreshRegistries();
  }, { done: "Registry removed" });

  const menu = (plugin: Plugin) => [
    { id: "update", label: "Update", icon: "arrow-up-right" as const },
    { id: "toggle", label: plugin.enabled ? "Turn off" : "Turn on", icon: plugin.enabled ? ("pause" as const) : ("play" as const) },
    { type: "separator" as const },
    { id: "remove", label: "Remove", icon: "trash" as const, danger: true },
  ];
  const choose = (plugin: Plugin, id: string) => (id === "remove" ? setRemoving(plugin) : void change.run(plugin, id));

  const failure = () => plugins.error() ?? registries.error();

  return (
    <View
      id="plugins"
      crumbs={[{ label: "Plugins" }]}
      error={failure() === undefined ? undefined : `The plugins could not be read. ${errorMessage(failure())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: () => (changed(), refreshRegistries()) },
        tab() === "installed"
          ? { id: "install", label: "Install plugin", icon: "plus", primary: true, onClick: () => setInstalling(true) }
          : { id: "add-registry", label: "Add registry", icon: "plus", primary: true, onClick: () => setAddingRegistry(true) },
      ]}
    >
      <Tabs
        variant="underline"
        label="Plugins"
        value={tab()}
        onChange={(next) => setTab(next as Tab)}
        items={[
          { id: "installed", label: "Installed", count: plugins()?.plugins.length },
          { id: "registries", label: "Registries", count: registries()?.registries.length },
        ]}
      >
        <TabPanel id="installed">
          <Show when={plugins()} fallback={<Skeleton width="50%" />}>
            {(answer) => (
              <Show
                when={answer().plugins.length > 0}
                fallback={
                  <Card>
                    <EmptyState icon="board" title="No plugins" description="A plugin adds views, host tabs, provisioners and settings.">
                      <Button variant="primary" icon="plus" onClick={() => setInstalling(true)}>
                        Install plugin
                      </Button>
                    </EmptyState>
                  </Card>
                }
              >
                <Card padding="none">
                  <RowList label="Installed plugins">
                    <For each={answer().plugins} keyed={(plugin) => plugin.id}>
                      {(plugin) => (
                        <ListRow
                          data-plugin={plugin().id}
                          icon="board"
                          title={plugin().name || plugin().id}
                          secondary={<span class="nc-mono">{`${plugin().id}@${plugin().version}`}</span>}
                          detail={contributionSummary(plugin()) || undefined}
                          muted={!plugin().enabled}
                          status={
                            <span class="nc-row nc-row--tight">
                              <Show when={!plugin().enabled}>
                                <Badge>off</Badge>
                              </Show>
                              <Show when={plugin().shell}>
                                <Badge tone="accent">shell</Badge>
                              </Show>
                            </span>
                          }
                          meta={relativeTime(dateOf(plugin().updatedAt ?? plugin().installedAt))}
                          menu={menu(plugin())}
                          onMenuSelect={(id) => choose(plugin(), id)}
                          persistentActions={
                            <RowActions label={`Actions for ${plugin().id}`} items={menu(plugin())} onSelect={(id) => choose(plugin(), id)} />
                          }
                        />
                      )}
                    </For>
                  </RowList>
                </Card>
              </Show>
            )}
          </Show>
        </TabPanel>
        <TabPanel id="registries">
          <Show when={registries()} fallback={<Skeleton width="50%" />}>
            {(answer) => (
              <Show
                when={answer().registries.length > 0}
                fallback={
                  <Card>
                    <EmptyState icon="archive" title="No registries" description="A registry is a signed index of plugin packages, served over HTTPS.">
                      <Button variant="primary" icon="plus" onClick={() => setAddingRegistry(true)}>
                        Add registry
                      </Button>
                    </EmptyState>
                  </Card>
                }
              >
                <Card padding="none">
                  <RowList label="Registries">
                    <For each={answer().registries} keyed={(registry) => registry.name}>
                      {(registry) => (
                        <ListRow
                          icon="archive"
                          title={registry().name}
                          secondary={<span class="nc-mono">{registry().url}</span>}
                          actions={
                            <Button size="sm" variant="ghost" icon="trash" onClick={() => setRemovingRegistry(registry())}>
                              Remove
                            </Button>
                          }
                        />
                      )}
                    </For>
                  </RowList>
                </Card>
              </Show>
            )}
          </Show>
        </TabPanel>
      </Tabs>

      <Show when={installing()}>
        <InstallDialog onClose={() => setInstalling(false)} onInstalled={changed} />
      </Show>
      <Show when={addingRegistry()}>
        <AddRegistryDialog onClose={() => setAddingRegistry(false)} onAdded={refreshRegistries} />
      </Show>
      <Show when={removing()} keyed>
        {(plugin) => <RemoveDialog plugin={plugin} onClose={() => setRemoving(undefined)} onRemoved={changed} />}
      </Show>
      <ConfirmDialog
        open={removingRegistry() !== undefined}
        title={`Remove ${removingRegistry()?.name ?? "the registry"}?`}
        description="Installed plugins stay. They no longer update from it."
        confirm="Remove registry"
        danger
        action={removeRegistry}
        onClose={() => setRemovingRegistry(undefined)}
      />
    </View>
  );
}

function InstallDialog(props: { onClose: () => void; onInstalled: () => void }): JSX.Element {
  const shell = useShell();
  const [source, setSource] = createSignal<"registry" | "file">("registry");
  const [reference, setReference] = createSignal("");
  const [file, setFile] = createSignal<File>();
  const install = createAction(async () => {
    const picked = file();
    if (source() === "file" && picked !== undefined) {
      await shell.rookery.plugins.installPackage({ package: new Uint8Array(await picked.arrayBuffer()) });
    } else {
      await shell.rookery.plugins.installFromRegistry({ reference: reference().trim() });
    }
  }, { done: "Plugin installed" });
  return (
    <FormDialog
      open
      title="Install plugin"
      submit="Install"
      action={install}
      disabled={source() === "registry" ? reference().trim() === "" : file() === undefined}
      onClose={props.onClose}
      onDone={props.onInstalled}
    >
      <div class="nc-stack">
        <Tabs
          label="Install from"
          value={source()}
          onChange={(next) => setSource(next as "registry" | "file")}
          items={[
            { id: "registry", label: "From a registry" },
            { id: "file", label: "From a file" },
          ]}
        />
        <Show
          when={source() === "registry"}
          fallback={
            <FileField label="Package" help="A .tar.gz with plugin.yaml at its root." accept=".tar.gz,application/gzip" onPick={setFile} />
          }
        >
          <TextField
            label="Plugin"
            mono
            placeholder="example@0.2.0"
            help="Its id, with a version or without one for the latest."
            value={reference()}
            onInput={(event) => setReference(event.currentTarget.value)}
          />
        </Show>
      </div>
    </FormDialog>
  );
}

function AddRegistryDialog(props: { onClose: () => void; onAdded: () => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const [url, setUrl] = createSignal("");
  const [key, setKey] = createSignal("");
  const add = createAction(async () => {
    await shell.rookery.registries.add({
      registry: { name: name().trim(), url: url().trim(), publicKey: new TextEncoder().encode(key().trim()) },
    });
  }, { done: "Registry added" });
  return (
    <FormDialog
      open
      title="Add registry"
      submit="Add registry"
      action={add}
      disabled={name().trim() === "" || !url().trim().startsWith("https://") || key().trim() === ""}
      onClose={props.onClose}
      onDone={props.onAdded}
    >
      <div class="nc-stack">
        <TextField label="Name" mono placeholder="lab" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
        <TextField
          label="URL"
          type="url"
          mono
          placeholder="https://plugins.example.test"
          value={url()}
          onInput={(event) => setUrl(event.currentTarget.value)}
        />
        <TextArea
          label="Public key"
          help="The key the registry signs its index with."
          rows={3}
          value={key()}
          onInput={(event) => setKey(event.currentTarget.value)}
        />
      </div>
    </FormDialog>
  );
}

function RemoveDialog(props: { plugin: Plugin; onClose: () => void; onRemoved: () => void }): JSX.Element {
  const shell = useShell();
  const [purge, setPurge] = createSignal(false);
  const remove = createAction(async () => {
    await shell.rookery.plugins.remove({ id: props.plugin.id, purge: purge() });
  }, { done: "Plugin removed" });
  return (
    <ConfirmDialog
      open
      title={`Remove ${props.plugin.name || props.plugin.id}?`}
      description="Its views and host tabs go at once. Hosts keep its features until their next apply."
      confirm="Remove plugin"
      danger
      action={remove}
      onClose={props.onClose}
      onDone={props.onRemoved}
    >
      <Switch label="Delete its data too" checked={purge()} onChange={setPurge} />
      <Show when={purge()}>
        <Callout tone="danger">Its records and documents are deleted, and a reinstall starts empty.</Callout>
      </Show>
    </ConfirmDialog>
  );
}

/** contributionSummary is what a plugin adds, in a few words. */
function contributionSummary(plugin: Plugin): string {
  if (plugin.contributesJson.length === 0) return "";
  try {
    const contributes = JSON.parse(new TextDecoder().decode(plugin.contributesJson)) as Record<string, unknown>;
    const parts: string[] = [];
    const count = (key: string, one: string, many: string) => {
      const value = contributes[key];
      if (Array.isArray(value) && value.length > 0) parts.push(`${value.length} ${value.length === 1 ? one : many}`);
    };
    count("views", "view", "views");
    count("hostTabs", "host tab", "host tabs");
    count("provisioners", "provisioner", "provisioners");
    count("features", "feature", "features");
    return parts.join(" · ");
  } catch {
    return "";
  }
}
