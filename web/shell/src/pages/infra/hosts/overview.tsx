// A host's overview: what it is and what it does. Its state and type, whether
// it is behind its profile, the actions it offers, and its labels. Applying is
// the primary action; renaming and removing are in the overflow.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Host } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  IconButton,
  ListRow,
  PropertyList,
  RowList,
  Skeleton,
  State,
  TextField,
  type PropertyItem,
} from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { ConfirmDialog, FormDialog } from "../../../parts/dialogs.tsx";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, hostState, hostTypeLabel, hostTypeTone, relativeTime } from "../format.ts";
import { jobPath } from "../jobs/list.tsx";
import { HostActions } from "./actions.tsx";
import { hostCrumbs, useHost } from "./domain.tsx";
import { hostPath } from "./paths.ts";

/** HostOverview is the first of a host's places. */
export function HostOverview(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const actions = createQuery(() => shell.rookery.hosts.listActions({ name: domain.name() }));
  const [renaming, setRenaming] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);

  const apply = createAction(
    async () => {
      const started = await shell.rookery.hosts.apply({ name: domain.name() });
      shell.navigate(jobPath(started.job));
    },
    { failed: "The apply did not start" },
  );

  const state = () => {
    const host = domain.host();
    return host === undefined ? undefined : hostState(host);
  };
  const behind = () => {
    const host = domain.host();
    return host !== undefined && host.appliedDigest !== "" && host.appliedDigest !== host.digest;
  };
  const never = () => {
    const host = domain.host();
    return host !== undefined && host.appliedDigest === "";
  };

  return (
    <View
      id="host-overview"
      crumbs={hostCrumbs(domain.name())}
      context={<Show when={state()}>{(shown) => <State status={shown().tone}>{shown().label}</State>}</Show>}
      actions={[
        { id: "refresh", label: "Read the host again", icon: "refresh-cw", onClick: domain.refresh },
        { id: "rename", label: "Rename", icon: "edit", overflow: true, onClick: () => setRenaming(true) },
        { id: "remove", label: "Remove", icon: "trash", overflow: true, danger: true, onClick: () => setRemoving(true) },
        {
          id: "apply",
          label: apply.busy() ? "Applying…" : "Apply",
          icon: "play",
          primary: true,
          disabled: apply.busy() || domain.host() === undefined,
          onClick: () => void apply.run(),
        },
      ]}
      aside={<Show when={domain.host()}>{(host) => <Details host={host()} />}</Show>}
      asideLabel="Details"
    >
      <Show when={domain.host()} fallback={<Skeleton width="40%" height="14px" />}>
        {(host) => (
          <>
            <Show when={never()}>
              <Callout tone="info" title="Not applied yet">
                Apply the profile to set the host up: its operator account, its features and the agent Rookery
                talks to.
              </Callout>
            </Show>
            <Show when={behind()}>
              <Callout tone="warning" title="Behind its profile">
                The profile changed since the last apply. Apply it to bring the host up to date.
              </Callout>
            </Show>
            <Show when={actions.error()}>
              {(error) => <Callout tone="danger">The actions could not be read: {error().message}</Callout>}
            </Show>
            <HostActions host={host().name} actions={actions()?.actions ?? []} />
            <Labels host={host()} onChanged={domain.refresh} />
          </>
        )}
      </Show>
      <RenameHost
        open={renaming()}
        host={domain.name()}
        onClose={() => setRenaming(false)}
        onRenamed={(renamed) => shell.navigate(hostPath(renamed))}
      />
      <RemoveHost open={removing()} host={domain.name()} onClose={() => setRemoving(false)} />
    </View>
  );
}

/** Details is the inspector beside the overview: what the host is now and how it is reached. */
function Details(props: { host: Host }): JSX.Element {
  const items = (): PropertyItem[] => {
    const host = props.host;
    const shown = hostState(host);
    const profile = host.profile;
    const port = profile === undefined || profile.port === 0 ? 22 : profile.port;
    const login = profile === undefined ? "" : `${profile.sshUser || "ops"}@${profile.address}${port === 22 ? "" : `:${port}`}`;
    return [
      { label: "State", value: <State status={shown.tone}>{shown.label}</State> },
      { label: "Type", value: <Badge tone={hostTypeTone(host.type)}>{hostTypeLabel(host.type)}</Badge> },
      { label: "SSH", value: <span class="nc-mono">{login || "Not known"}</span> },
      { label: "Tailnet", value: host.address === "" ? (profile?.tailnet ? "Joins on apply" : "No") : <span class="nc-mono">{host.address}</span> },
      { label: "Applied", value: relativeTime(dateOf(host.appliedAt)) || "Never" },
      { label: "Last seen", value: relativeTime(dateOf(host.lastSeenAt)) || "Never" },
      { label: "Added", value: relativeTime(dateOf(host.createdAt)) },
      { label: "Features", value: String(profile?.features.length ?? 0) },
    ];
  };
  return (
    <div class="shell-view">
      <PropertyList items={items()} />
    </div>
  );
}

/** Labels are a host's key-value pairs, which roll-outs and filters select on. */
function Labels(props: { host: Host; onChanged: () => void }): JSX.Element {
  const shell = useShell();
  const [key, setKey] = createSignal("");
  const [value, setValue] = createSignal("");
  const save = createAction(
    async (set: Record<string, string>, remove: string[]) => {
      await shell.rookery.hosts.setLabels({ name: props.host.name, set, remove });
      props.onChanged();
    },
    { failed: "The labels did not change" },
  );
  const entries = () => Object.entries(props.host.labels).sort(([a], [b]) => a.localeCompare(b));
  const add = async (event: Event) => {
    event.preventDefault();
    const name = key().trim();
    if (name === "") return;
    if (await save.run({ [name]: value().trim() }, [])) {
      setKey("");
      setValue("");
    }
  };

  return (
    <Card title="Labels" count={entries().length}>
      <div class="nc-stack">
        <Show
          when={entries().length > 0}
          fallback={<p class="nc-meta">No labels. Roll-outs select hosts by them, such as role=desk or site=lab.</p>}
        >
          <RowList label="Labels" variant="inline">
            <For each={entries()}>
              {([name, label]) => (
                <ListRow
                  title={<span class="nc-code">{name}</span>}
                  secondary={label === "" ? undefined : <span class="nc-code">{label}</span>}
                  actions={
                    <IconButton
                      size="sm"
                      label={`Remove the label ${name}`}
                      icon="close"
                      disabled={save.busy()}
                      onClick={() => void save.run({}, [name])}
                    />
                  }
                />
              )}
            </For>
          </RowList>
        </Show>
        <form class="shell-form__row" onSubmit={(event) => void add(event)}>
          <TextField label="Label" value={key()} placeholder="role" onInput={(event) => setKey(event.currentTarget.value)} />
          <TextField label="Value" value={value()} placeholder="desk" onInput={(event) => setValue(event.currentTarget.value)} />
          <Button type="submit" disabled={save.busy() || key().trim() === ""}>
            Add
          </Button>
        </form>
      </div>
    </Card>
  );
}

/** RenameHost renames a host, which keeps its grants, labels and machine. */
function RenameHost(props: { open: boolean; host: string; onClose: () => void; onRenamed: (name: string) => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  let renamed = "";
  const rename = createAction(async () => {
    const answer = await shell.rookery.hosts.rename({ name: props.host, newName: name().trim() });
    renamed = answer.host?.name ?? name().trim();
  });
  return (
    <FormDialog
      open={props.open}
      title={`Rename ${props.host}`}
      description="The host keeps its grants, its labels and its machine."
      submit="Rename"
      action={rename}
      disabled={name().trim() === "" || name().trim() === props.host}
      onClose={() => {
        setName("");
        props.onClose();
      }}
      onDone={() => props.onRenamed(renamed)}
    >
      <TextField label="New name" value={name()} placeholder={props.host} onInput={(event) => setName(event.currentTarget.value)} />
    </FormDialog>
  );
}

/** RemoveHost forgets a host. The machine itself keeps running. */
function RemoveHost(props: { open: boolean; host: string; onClose: () => void }): JSX.Element {
  const shell = useShell();
  const remove = createAction(() => shell.rookery.hosts.remove({ name: props.host }), { done: `Removed ${props.host}` });
  return (
    <ConfirmDialog
      open={props.open}
      title={`Remove ${props.host}?`}
      description="Rookery forgets the host, its labels and its grants. What runs on the machine is not stopped; add the host again to reach it."
      confirm="Remove host"
      danger
      action={remove}
      onClose={props.onClose}
      onDone={() => shell.navigate("/hosts")}
    />
  );
}
