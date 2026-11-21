// A space's overview: its newest documents and the people who may use it.
// Renaming, archiving and deleting are the header's, for those who manage it.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { type Grant, Level } from "@rookery/proto";
import { Badge, Card, EmptyState, IconButton, ListRow, RowList, Select, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { RowActions } from "../../parts/row-actions.tsx";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { spacePath } from "../../spaces.ts";
import { dateOf, relativeTime } from "../fleet/format.ts";
import { type SpaceDomainValue, spaceCrumbs, useSpace } from "./domain.tsx";

/** LEVELS are what a member may do in a space. */
const LEVELS = [
  { value: String(Level.VIEW), label: "View" },
  { value: String(Level.USE), label: "Use" },
  { value: String(Level.MANAGE), label: "Manage" },
];

/** SpaceOverview is the first of a space's places. */
export function SpaceOverview(): JSX.Element {
  const shell = useShell();
  const domain = useSpace();
  const documents = createLive(() => shell.rookery.documents.list({ parent: `space://${domain.id}`, limit: 5 }).catch(() => undefined));
  const [renaming, setRenaming] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const archive = createAction(
    async () => {
      const archived = !(domain.space()?.archived ?? false);
      await shell.rookery.spaces.archive({ space: domain.id, archived });
      domain.refresh();
      if (archived) shell.navigate("/");
    },
    { failed: "The space did not change" },
  );
  const remove = createAction(async () => {
    await shell.rookery.spaces.delete({ space: domain.id });
    void shell.spaces.refresh();
  }, { done: "Space deleted" });

  return (
    <View
      id="space-overview"
      crumbs={spaceCrumbs(domain)}
      context={domain.space()?.archived ? <Badge>Archived</Badge> : undefined}
      actions={
        domain.manages()
          ? [
              { id: "rename", label: "Rename", icon: "edit", overflow: true, onClick: () => setRenaming(true) },
              { id: "archive", label: domain.space()?.archived ? "Unarchive" : "Archive", icon: "archive", overflow: true, onClick: () => void archive.run() },
              { id: "delete", label: "Delete", icon: "trash", overflow: true, danger: true, onClick: () => setDeleting(true) },
            ]
          : []
      }
    >
      <Show when={(documents()?.documents.length ?? 0) > 0}>
        <Card title="Documents" padding="none" action={<a class="shell-link nc-meta" href={spacePath(domain.id, "documents")}>All</a>}>
          <RowList label="Documents">
            <For each={documents()?.documents ?? []} keyed={(document) => document.id}>
              {(document) => (
                <ListRow
                  icon="file"
                  title={<span class="nc-mono">{document().id}</span>}
                  label={document().id}
                  meta={relativeTime(dateOf(document().updatedAt))}
                  href={`${spacePath(domain.id, "documents")}?document=${encodeURIComponent(document().id)}`}
                />
              )}
            </For>
          </RowList>
        </Card>
      </Show>
      <Members domain={domain} />
      <Show when={renaming()}>
        <RenameSpace domain={domain} onClose={() => setRenaming(false)} />
      </Show>
      <ConfirmDialog
        open={deleting()}
        title={`Delete ${domain.space()?.name ?? "the space"}?`}
        description="Its records and documents go with it."
        confirm="Delete space"
        danger
        action={remove}
        onClose={() => setDeleting(false)}
        onDone={() => shell.navigate("/")}
      />
    </View>
  );
}

/** Members are the grants on the space: who may view, use or manage it. */
function Members(props: { domain: SpaceDomainValue }): JSX.Element {
  const shell = useShell();
  const resource = `space://${props.domain.id}`;
  const grants = createLive(() => shell.rookery.access.listGrants({ resource, limit: 200 }).catch(() => undefined));
  // Names come from the people list, which only admins read; others see ids.
  const users = createLive(() => shell.rookery.users.list({ limit: 500 }).catch(() => undefined));
  const groups = createLive(() => shell.rookery.groups.list({ limit: 500 }).catch(() => undefined));
  const [adding, setAdding] = createSignal(false);
  const revoke = createAction(async (grant: Grant) => {
    await shell.rookery.access.revoke({ resource: grant.resource, grantee: grant.grantee });
    void grants.refresh().catch(() => undefined);
  }, { failed: "The member was not removed" });

  const name = (grant: Grant): string => {
    const who = grant.grantee?.who;
    if (who?.case === "userId") {
      const user = users()?.users.find((one) => one.id === who.value);
      return user?.name || user?.email || who.value;
    }
    if (who?.case === "groupId") return groups()?.groups.find((one) => one.id === who.value)?.name ?? who.value;
    if (who?.case === "everyone") return "Everyone";
    return "—";
  };
  const level = (value: Level) => LEVELS.find((one) => one.value === String(value))?.label ?? "Admin";
  const canAdd = () => props.domain.manages() && users() !== undefined;
  const rows = () => grants()?.grants ?? [];

  return (
    <Show when={grants() !== undefined && (rows().length > 0 || canAdd())}>
      <Card
        title="Members"
        count={rows().length > 0 ? rows().length : undefined}
        padding="none"
        action={
          <Show when={canAdd()}>
            <IconButton size="sm" icon="plus" label="Add member" onClick={() => setAdding(true)} />
          </Show>
        }
      >
        <Show when={rows().length > 0} fallback={<EmptyState size="sm" title="No members" />}>
          <RowList label="Members">
            <For each={rows()}>
              {(grant) => {
                const menu = [{ id: "remove", label: "Remove", icon: "trash" as const, danger: true }];
                return (
                  <ListRow
                    icon={grant.grantee?.who.case === "groupId" ? "users" : "user"}
                    title={name(grant)}
                    meta={level(grant.level)}
                    menu={props.domain.manages() ? menu : undefined}
                    menuLabel={`Actions for ${name(grant)}`}
                    onMenuSelect={() => void revoke.run(grant)}
                    persistentActions={
                      props.domain.manages() ? (
                        <RowActions label={`Actions for ${name(grant)}`} items={menu} onSelect={() => void revoke.run(grant)} />
                      ) : undefined
                    }
                  />
                );
              }}
            </For>
          </RowList>
        </Show>
      </Card>
      <Show when={adding()}>
        <AddMember
          resource={resource}
          users={(users()?.users ?? []).map((user) => ({ value: user.id, label: user.name || user.email }))}
          groups={(groups()?.groups ?? []).map((group) => ({ value: group.id, label: group.name }))}
          onClose={() => setAdding(false)}
          onAdded={() => void grants.refresh().catch(() => undefined)}
        />
      </Show>
    </Show>
  );
}

/** AddMember grants a person or a group a level on the space. */
function AddMember(props: {
  resource: string;
  users: Array<{ value: string; label: string }>;
  groups: Array<{ value: string; label: string }>;
  onClose: () => void;
  onAdded: () => void;
}): JSX.Element {
  const shell = useShell();
  const [who, setWho] = createSignal("");
  const [level, setLevel] = createSignal(String(Level.USE));
  const options = () => [
    ...props.users.map((user) => ({ value: `user:${user.value}`, label: user.label })),
    ...props.groups.map((group) => ({ value: `group:${group.value}`, label: group.label, description: "Group" })),
    { value: "everyone", label: "Everyone signed in" },
  ];
  const add = createAction(async () => {
    const chosen = who();
    const grantee =
      chosen === "everyone"
        ? { case: "everyone" as const, value: true }
        : chosen.startsWith("group:")
          ? { case: "groupId" as const, value: chosen.slice(6) }
          : { case: "userId" as const, value: chosen.slice(5) };
    await shell.rookery.access.grant({ resource: props.resource, grantee: { who: grantee }, level: Number(level()) as Level });
  });
  return (
    <FormDialog open title="Add member" submit="Add" action={add} disabled={who() === ""} onClose={props.onClose} onDone={props.onAdded}>
      <Select label="Who" placeholder="Choose a person or group" value={who()} options={options()} onChange={setWho} />
      <Select label="Level" value={level()} options={LEVELS} onChange={setLevel} />
    </FormDialog>
  );
}

function RenameSpace(props: { domain: SpaceDomainValue; onClose: () => void }): JSX.Element {
  const shell = useShell();
  const current = props.domain.space()?.name ?? "";
  const [name, setName] = createSignal(current);
  const rename = createAction(async () => {
    await shell.rookery.spaces.rename({ space: props.domain.id, name: name().trim() });
  });
  return (
    <FormDialog
      open
      title="Rename space"
      submit="Rename"
      action={rename}
      disabled={name().trim() === "" || name().trim() === current}
      onClose={props.onClose}
      onDone={props.domain.refresh}
    >
      <TextField label="Name" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
    </FormDialog>
  );
}
