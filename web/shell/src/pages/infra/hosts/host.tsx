// A host's page: what it is, what it should be, and the ways in — its actions,
// its files, its ports, a terminal and its desktop. The tabs plugins add come
// after those, each drawn by the plugin's own client half.
import { For, Show, createEffect, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import { createQuery } from "@rookery/client/solid";
import { HostType, type Host } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Badge, Button, Callout, Card, Field, IconButton, Input, Page, Spinner } from "../../../ui.tsx";
import { HostTabMount } from "../../../plugin/mount.tsx";
import {
  errorMessage,
  hostState,
  hostTypeLabel,
  hostTypeName,
  hostTypeTone,
  relativeTime,
  dateOf,
} from "../format.ts";
import { Dialog, PropertyList, StateText, Tabs, type PropertyItem } from "../parts.tsx";
import { HostActions } from "./actions.tsx";
import { HostFiles } from "./files.tsx";
import { HostPorts } from "./ports.tsx";
import { HostProfile } from "./profile.tsx";

export function HostPage(): JSX.Element {
  const shell = useShell();
  const params = useParams<{ name: string }>();
  const name = () => params.name ?? "";

  const host = createQuery(() => shell.rookery.hosts.get({ name: name() }));
  const actions = createQuery(() => shell.rookery.hosts.listActions({ name: name() }));
  const [tab, setTab] = createSignal<string>("overview");
  const [editing, setEditing] = createSignal(false);
  const [renaming, setRenaming] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  createEffect(
    () => name(),
    (_next, previous) => {
      if (previous === undefined) return;
      setTab("overview");
      void host.refresh().catch(() => undefined);
      void actions.refresh().catch(() => undefined);
    },
  );

  onSettled(() =>
    shell.connection.on("rookery/hosts.changed", () => {
      void host.refresh().catch(() => undefined);
    }),
  );

  const current = (): Host | undefined => host()?.host;
  const state = () => (current() === undefined ? undefined : hostState(current()!));

  const pluginTabs = () => {
    const type = hostTypeName(current()?.type ?? HostType.SSH);
    return shell.plugins.hostTabs().filter((entry) => entry.types.length === 0 || entry.types.includes(type));
  };

  const tabs = () => [
    { id: "overview", label: "Overview" },
    { id: "actions", label: "Actions", count: actions()?.actions.length ?? 0 },
    { id: "files", label: "Files" },
    { id: "ports", label: "Ports" },
    ...pluginTabs().map((entry) => ({ id: `${entry.plugin}/${entry.id}`, label: entry.label })),
  ];

  const apply = async (): Promise<void> => {
    if (busy()) return;
    setBusy(true);
    setFailure(undefined);
    try {
      const started = await shell.rookery.hosts.apply({ name: name() });
      shell.navigate(`/jobs/${encodeURIComponent(started.job)}`);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  const overview = (): PropertyItem[] => {
    const shown = current();
    if (shown === undefined) return [];
    const applied = hostState(shown);
    return [
      { label: "State", value: <StateText tone={applied.tone}>{applied.label}</StateText> },
      { label: "Type", value: <Badge tone={hostTypeTone(shown.type)}>{hostTypeLabel(shown.type)}</Badge> },
      {
        label: "Address",
        value: <span class="nc-code">{shown.address || shown.profile?.address || "Not known yet"}</span>,
      },
      { label: "Applied", value: relativeTime(dateOf(shown.appliedAt)) || "Never" },
      { label: "Last seen", value: relativeTime(dateOf(shown.lastSeenAt)) || "Never" },
      { label: "Added", value: relativeTime(dateOf(shown.createdAt)) },
      { label: "Digest", value: <span class="nc-code">{shown.digest.slice(0, 12)}</span> },
    ];
  };

  const profileItems = (): PropertyItem[] => {
    const profile = current()?.profile;
    if (profile === undefined) return [];
    const port = profile.port === 0 ? 22 : profile.port;
    return [
      {
        label: "SSH",
        value: (
          <span class="nc-code">
            {profile.sshUser || "ops"}@{profile.address}
            {port === 22 ? "" : `:${port}`}
          </span>
        ),
      },
      { label: "Operator", value: <span class="nc-code">{profile.operator || "ops"}</span> },
      { label: "Tailnet", value: profile.tailnet ? "Joined" : "Not joined" },
      {
        label: "Desktop",
        value:
          profile.desktop === undefined
            ? ""
            : `${profile.desktop.user || "dev"} · ${profile.desktop.width}×${profile.desktop.height}${profile.desktop.gpu ? " · GPU" : ""}`,
      },
      { label: "Registries", value: profile.registries.join(", ") },
      {
        label: "Features",
        value: (
          <span class="nc-row">
            <For each={profile.features}>
              {feature => (
                <Badge tone="outline">
                  {feature.plugin === "" ? feature.feature : `${feature.plugin}/${feature.feature}`}
                </Badge>
              )}
            </For>
          </span>
        ),
      },
    ];
  };

  const behind = () => {
    const shown = current();
    return shown !== undefined && shown.appliedDigest !== "" && shown.digest !== "" && shown.appliedDigest !== shown.digest;
  };

  return (
    <Page
      title={current()?.name ?? name()}
      description="A machine Rookery runs. Its profile is what an apply makes of it."
      actions={
        <>
          <Show when={state()}>
            {shown => <StateText tone={shown().tone}>{shown().label}</StateText>}
          </Show>
          <IconButton
            label="Open a terminal"
            glyph="hosts"
            onClick={() => shell.navigate(`/terminals?host=${encodeURIComponent(name())}`)}
          />
          <IconButton
            label="Open the desktop"
            glyph="server"
            onClick={() => shell.navigate(`/hosts/${encodeURIComponent(name())}/desktop`)}
          />
          <IconButton label="Edit the profile" glyph="settings" onClick={() => setEditing(true)} />
          <Button variant="ghost" onClick={() => setRenaming(true)}>
            Rename
          </Button>
          <Button variant="ghost" onClick={() => setRemoving(true)}>
            Remove
          </Button>
          <Button variant="primary" onClick={() => void apply()} disabled={busy() || current() === undefined}>
            {busy() ? "Applying…" : "Apply"}
          </Button>
        </>
      }
    >
      <Show when={host.error()}>
        {error => (
          <Callout tone="danger">
            The host could not be read: {error().message}
          </Callout>
        )}
      </Show>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show
        when={current() !== undefined}
        fallback={
          <Show when={host.pending()}>
            <span class="nc-row">
              <Spinner /> Reading the host…
            </span>
          </Show>
        }
      >
        <Show when={behind()}>
          <Callout tone="warning">The profile changed since the host was last applied.</Callout>
        </Show>
        <Tabs label={`Sections of ${name()}`} items={tabs()} value={tab()} onChange={setTab} />
        <Show when={tab() === "overview"}>
          <div class="infra-grid">
            <Card title="Overview">
              <PropertyList items={overview()} />
            </Card>
            <Card
              title="Profile"
            >
              <PropertyList items={profileItems()} />
            </Card>
          </div>
          <Labels host={current()!} onChanged={() => void host.refresh().catch(() => undefined)} />
        </Show>
        <Show when={tab() === "actions"}>
          <HostActions host={name()} actions={actions()?.actions ?? []} />
        </Show>
        <Show when={tab() === "files"}>
          <HostFiles host={name()} />
        </Show>
        <Show when={tab() === "ports"}>
          <HostPorts host={name()} />
        </Show>
        <For each={pluginTabs().filter(entry => `${entry.plugin}/${entry.id}` === tab())}>
          {entry => <HostTabMount half={entry.half} id={entry.id} host={name()} />}
        </For>
      </Show>
      <Show when={current()}>
        {shown => (
          <>
            <HostProfile open={editing()} host={shown()} onClose={() => setEditing(false)} onSaved={() => {
              setEditing(false);
              void host.refresh().catch(() => undefined);
            }} />
            <RenameHost
              open={renaming()}
              host={shown().name}
              onClose={() => setRenaming(false)}
              onRenamed={renamed => {
                setRenaming(false);
                shell.navigate(`/hosts/${encodeURIComponent(renamed)}`);
              }}
            />
            <RemoveHost
              open={removing()}
              host={shown().name}
              onClose={() => setRemoving(false)}
              onRemoved={() => shell.navigate("/hosts")}
            />
          </>
        )}
      </Show>
    </Page>
  );
}

/** Labels is a host's labels, which roll-outs and filters select on. */
function Labels(props: { host: Host; onChanged: () => void }): JSX.Element {
  const shell = useShell();
  const [key, setKey] = createSignal("");
  const [value, setValue] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const save = async (labels: Record<string, string>, remove: string[]): Promise<void> => {
    setBusy(true);
    setFailure(undefined);
    try {
      await shell.rookery.hosts.setLabels({ name: props.host.name, set: labels, remove });
      props.onChanged();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  const entries = () => Object.entries(props.host.labels).sort(([a], [b]) => a.localeCompare(b));

  return (
    <Card
      title="Labels"
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show
        when={entries().length > 0}
        fallback={
          <p class="nc-empty__desc">No labels. Roll-outs select hosts by them, such as role=desk or site=lab.</p>
        }
      >
        <ul class="nc-list">
          <For each={entries()}>
            {([name, label]) => (
              <li class="nc-list__row" data-label={name}>
                <span class="nc-code">{name}</span>
                <span class="infra-grow">{label}</span>
                <IconButton label={`Remove the label ${name}`} glyph="close" onClick={() => void save({}, [name])} />
              </li>
            )}
          </For>
        </ul>
      </Show>
      <form
        class="nc-row"
        onSubmit={event => {
          event.preventDefault();
          if (key().trim() === "") return;
          const labels = { [key().trim()]: value().trim() };
          setKey("");
          setValue("");
          void save(labels, []);
        }}
      >
        <Field label="Label">
          <Input value={key()} placeholder="role" onInput={setKey} />
        </Field>
        <Field label="Value">
          <Input value={value()} placeholder="desk" onInput={setValue} />
        </Field>
        <Button type="submit" disabled={busy() || key().trim() === ""}>
          Add label
        </Button>
      </form>
    </Card>
  );
}

/** RenameHost renames a host, which keeps its grants. */
function RenameHost(props: {
  open: boolean;
  host: string;
  onClose: () => void;
  onRenamed: (name: string) => void;
}): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal(props.host);
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const submit = async (): Promise<void> => {
    if (busy()) return;
    setBusy(true);
    try {
      const renamed = await shell.rookery.hosts.rename({ name: props.host, newName: name().trim() });
      props.onRenamed(renamed.host?.name ?? name().trim());
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      title={`Rename ${props.host}`}
      description="The host keeps its grants, its labels and its machine."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void submit()} disabled={busy() || name().trim() === ""}>
            Rename
          </Button>
        </>
      }
    >
      <Field label="New name">
        <Input value={name()} onInput={setName} />
      </Field>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
    </Dialog>
  );
}

/** RemoveHost removes a host's record, not the machine. */
function RemoveHost(props: {
  open: boolean;
  host: string;
  onClose: () => void;
  onRemoved: () => void;
}): JSX.Element {
  const shell = useShell();
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const submit = async (): Promise<void> => {
    if (busy()) return;
    setBusy(true);
    try {
      await shell.rookery.hosts.remove({ name: props.host });
      props.onRemoved();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      title={`Remove ${props.host}?`}
      description="Rookery forgets the host, its labels and its grants. The machine itself stays as it is."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button variant="danger" onClick={() => void submit()} disabled={busy()} data-part="confirm-remove-host">
            Remove host
          </Button>
        </>
      }
    >
      <Callout tone="warning">What runs on the machine is not stopped. Add the host again to reach it.</Callout>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
    </Dialog>
  );
}
