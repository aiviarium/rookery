// A host's overview: the actions it offers and its last jobs, with what it is
// and how it is reached beside them. Its state shows beside the path only
// while it is out of the ordinary; applying, renaming and removing are the
// header's.
import { Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Host } from "@rookery/proto";
import { Badge, Callout, Card, IconButton, PropertyList, type PropertyItem, Skeleton, State, TextArea, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../../parts/dialogs.tsx";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, hostIssue, hostTypeLabel, relativeTime } from "../format.ts";
import { JobRows, jobPath } from "../jobs/rows.tsx";
import { HostActions } from "./actions.tsx";
import { hostCrumbs, useHost } from "./domain.tsx";
import { hostPath, hostResource } from "./paths.ts";

/** HostOverview is the first of a host's places. */
export function HostOverview(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const actions = createQuery(() => shell.rookery.hosts.listActions({ name: domain.name() }));
  const jobs = createLive(() => shell.rookery.jobs.list({ resource: hostResource(domain.name()), limit: 5 }), {
    events: ["rookery/jobs."],
  });
  const [renaming, setRenaming] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);
  const [labelling, setLabelling] = createSignal(false);

  const apply = createAction(
    async () => {
      const started = await shell.rookery.hosts.apply({ name: domain.name() });
      shell.navigate(jobPath(started.job));
    },
    { failed: "The apply did not start" },
  );
  const issue = () => {
    const host = domain.host();
    return host === undefined ? undefined : hostIssue(host);
  };

  return (
    <View
      id="host-overview"
      crumbs={hostCrumbs(domain.name())}
      context={<Show when={issue()}>{(shown) => <State status={shown().tone}>{shown().label}</State>}</Show>}
      actions={[
        { id: "apply", label: "Apply profile", icon: "play", disabled: apply.busy() || domain.host() === undefined, onClick: () => void apply.run() },
        { id: "rename", label: "Rename", icon: "edit", overflow: true, onClick: () => setRenaming(true) },
        { id: "remove", label: "Remove", icon: "trash", overflow: true, danger: true, onClick: () => setRemoving(true) },
      ]}
      aside={<Show when={domain.host()}>{(host) => <Details host={host()} onLabels={() => setLabelling(true)} />}</Show>}
      asideLabel="Details"
    >
      <Show when={domain.host()} fallback={<Skeleton width="40%" height="14px" />}>
        {(host) => (
          <>
            <Show when={actions.error()}>{(error) => <Callout tone="danger">{error().message}</Callout>}</Show>
            <HostActions host={host().name} actions={actions()?.actions ?? []} />
            <Show when={(jobs()?.jobs.length ?? 0) > 0}>
              <Card title="Jobs" padding="none" action={<a class="shell-link nc-meta" href={hostPath(host().name, "jobs")}>All</a>}>
                <JobRows jobs={jobs()?.jobs} label={`Jobs on ${host().name}`} empty={{ title: "No jobs" }} />
              </Card>
            </Show>
          </>
        )}
      </Show>
      <RenameHost open={renaming()} host={domain.name()} onClose={() => setRenaming(false)} onRenamed={(renamed) => shell.navigate(hostPath(renamed))} />
      <RemoveHost open={removing()} host={domain.name()} onClose={() => setRemoving(false)} />
      <Show when={labelling() ? domain.host() : undefined} keyed>
        {(host) => <LabelsDialog host={host} onClose={() => setLabelling(false)} onChanged={domain.refresh} />}
      </Show>
    </View>
  );
}

/** Details is the inspector beside the overview: what the host is and how it is reached. */
function Details(props: { host: Host; onLabels: () => void }): JSX.Element {
  const items = (): PropertyItem[] => {
    const host = props.host;
    const profile = host.profile;
    const port = profile === undefined || profile.port === 0 ? 22 : profile.port;
    const login = profile === undefined ? "" : `${profile.sshUser || "ops"}@${profile.address}${port === 22 ? "" : `:${port}`}`;
    const labels = Object.entries(host.labels).sort(([a], [b]) => a.localeCompare(b));
    const out: PropertyItem[] = [
      { label: "Type", value: hostTypeLabel(host.type) },
      { label: "SSH", value: <span class="nc-mono">{login || "—"}</span> },
    ];
    if (host.address !== "") out.push({ label: "Tailnet", value: <span class="nc-mono">{host.address}</span> });
    if (host.appliedAt !== undefined) out.push({ label: "Applied", value: relativeTime(dateOf(host.appliedAt)) });
    if (host.lastSeenAt !== undefined) out.push({ label: "Last seen", value: relativeTime(dateOf(host.lastSeenAt)) });
    out.push({
      label: "Labels",
      value: (
        <span class="shell-labels">
          {labels.map(([key, value]) => (
            <Badge tone="outline">
              <span class="nc-mono">{value === "" ? key : `${key}=${value}`}</span>
            </Badge>
          ))}
          <IconButton size="sm" icon="edit" label="Edit labels" onClick={props.onLabels} />
        </span>
      ),
    });
    return out;
  };
  return (
    <div class="shell-view">
      <PropertyList items={items()} />
    </div>
  );
}

/** LabelsDialog edits a host's labels as lines of key=value, which roll-outs and filters select on. */
function LabelsDialog(props: { host: Host; onClose: () => void; onChanged: () => void }): JSX.Element {
  const shell = useShell();
  const initial = untrack(() =>
    Object.entries(props.host.labels)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([key, value]) => (value === "" ? key : `${key}=${value}`))
      .join("\n"),
  );
  const [text, setText] = createSignal(initial);
  const parsed = () => {
    const out: Record<string, string> = {};
    for (const line of text().split("\n")) {
      const trimmed = line.trim();
      if (trimmed === "") continue;
      const at = trimmed.indexOf("=");
      if (at < 0) out[trimmed] = "";
      else out[trimmed.slice(0, at).trim()] = trimmed.slice(at + 1).trim();
    }
    return out;
  };
  const save = createAction(async () => {
    const next = parsed();
    const remove = Object.keys(props.host.labels).filter((key) => !(key in next));
    await shell.rookery.hosts.setLabels({ name: props.host.name, set: next, remove });
  });
  return (
    <FormDialog open title="Labels" submit="Save" action={save} onClose={props.onClose} onDone={props.onChanged}>
      <TextArea
        label="Labels"
        info="One per line, as key=value, such as role=desk. Roll-outs select hosts by them."
        rows={6}
        value={text()}
        onInput={(event) => setText(event.currentTarget.value)}
      />
    </FormDialog>
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
      submit="Rename"
      action={rename}
      disabled={name().trim() === "" || name().trim() === props.host}
      onClose={() => {
        setName("");
        props.onClose();
      }}
      onDone={() => props.onRenamed(renamed)}
    >
      <TextField label="Name" value={name()} placeholder={props.host} onInput={(event) => setName(event.currentTarget.value)} />
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
      description="What runs on the machine keeps running."
      confirm="Remove host"
      danger
      action={remove}
      onClose={props.onClose}
      onDone={() => shell.navigate("/hosts")}
    />
  );
}
