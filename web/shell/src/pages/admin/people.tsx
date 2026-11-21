// People: who may sign in, the groups that gather them, and the grants that say
// what each holds. One tab each; the header's primary action is the tab's own.
import { For, Show, createMemo, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Level, type Grant, type Group, type User } from "@rookery/proto";
import {
  Avatar,
  Badge,
  Button,
  Card,
  DataTable,
  type DataColumn,
  Dialog,
  EmptyState,
  IconButton,
  ListRow,
  RowList,
  Select,
  Skeleton,
  TabPanel,
  Tabs,
  TextField,
} from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { CopyField } from "../../parts/copy-field.tsx";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { createTab } from "../../parts/tab.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";

/** LEVELS are the levels a grant gives, in order. */
const LEVELS = [
  { value: String(Level.VIEW), label: "View", description: "See it." },
  { value: String(Level.USE), label: "Use", description: "Open terminals, run actions." },
  { value: String(Level.MANAGE), label: "Manage", description: "Change and remove it." },
];

const TABS = ["users", "groups", "grants"] as const;
type Tab = (typeof TABS)[number];

/** PeoplePage is the people, their groups and the grants on a resource. */
export function PeoplePage(): JSX.Element {
  const shell = useShell();
  const users = createLive(() => shell.rookery.users.list({ limit: 500 }));
  const groups = createLive(() => shell.rookery.groups.list({ limit: 500 }));
  const [tab, setTab] = createTab<Tab>(TABS);

  const [inviting, setInviting] = createSignal(false);
  const [link, setLink] = createSignal<{ email: string; url: string }>();
  const [creatingGroup, setCreatingGroup] = createSignal(false);
  const [granting, setGranting] = createSignal(false);
  const [grantsVersion, setGrantsVersion] = createSignal(0);

  const userRows = () => users()?.users ?? [];
  const groupRows = () => groups()?.groups ?? [];
  const refreshUsers = () => void users.refresh().catch(() => undefined);
  const refreshGroups = () => void groups.refresh().catch(() => undefined);

  const primary = () => {
    switch (tab()) {
      case "users":
        return { id: "invite", label: "Invite", icon: "plus" as const, primary: true, onClick: () => setInviting(true) };
      case "groups":
        return { id: "new-group", label: "New group", icon: "plus" as const, primary: true, onClick: () => setCreatingGroup(true) };
      default:
        return { id: "grant", label: "Grant access", icon: "plus" as const, primary: true, onClick: () => setGranting(true) };
    }
  };

  const failure = () => users.error() ?? groups.error();

  return (
    <View
      id="people"
      crumbs={[{ label: "People" }]}
      error={failure() === undefined ? undefined : `The people could not be read. ${errorMessage(failure())}`}
      actions={[
        {
          id: "refresh",
          label: "Refresh",
          icon: "refresh-cw",
          onClick: () => {
            refreshUsers();
            refreshGroups();
            setGrantsVersion((version) => version + 1);
          },
        },
        primary(),
      ]}
    >
      <Tabs
        variant="underline"
        label="People"
        value={tab()}
        onChange={(next) => setTab(next as Tab)}
        items={[
          { id: "users", label: "Users", count: users() === undefined ? undefined : userRows().length },
          { id: "groups", label: "Groups", count: groups() === undefined ? undefined : groupRows().length },
          { id: "grants", label: "Grants" },
        ]}
      >
        <TabPanel id="users">
          <UsersTab users={users() === undefined ? undefined : userRows()} onChanged={refreshUsers} onLink={setLink} onInvite={() => setInviting(true)} />
        </TabPanel>
        <TabPanel id="groups">
          <GroupsTab
            groups={groups() === undefined ? undefined : groupRows()}
            onChanged={refreshGroups}
            onCreate={() => setCreatingGroup(true)}
          />
        </TabPanel>
        <TabPanel id="grants">
          <GrantsTab users={userRows()} groups={groupRows()} version={grantsVersion()} onGrant={() => setGranting(true)} />
        </TabPanel>
      </Tabs>

      <Show when={inviting()}>
        <InviteDialog
          onClose={() => setInviting(false)}
          onMade={(made) => {
            setLink(made);
            refreshUsers();
          }}
        />
      </Show>
      <Show when={link()}>
        {(made) => (
          <Dialog
            open
            onClose={() => setLink(undefined)}
            title="Sign-in link"
            description={`For ${made().email}. It works once and lasts a day. Send it to them yourself.`}
            footer={
              <Button variant="primary" onClick={() => setLink(undefined)}>
                Done
              </Button>
            }
          >
            <CopyField label="Link" value={made().url} />
          </Dialog>
        )}
      </Show>
      <Show when={creatingGroup()}>
        <NewGroupDialog onClose={() => setCreatingGroup(false)} onCreated={refreshGroups} />
      </Show>
      <Show when={granting()}>
        <GrantDialog
          users={userRows()}
          groups={groupRows()}
          onClose={() => setGranting(false)}
          onGranted={() => setGrantsVersion((version) => version + 1)}
        />
      </Show>
    </View>
  );
}

function UsersTab(props: {
  users: readonly User[] | undefined;
  onChanged: () => void;
  onLink: (made: { email: string; url: string }) => void;
  onInvite: () => void;
}): JSX.Element {
  const shell = useShell();
  const me = () => shell.signIn.user()?.id;
  const change = createAction(async (user: User, id: string) => {
    switch (id) {
      case "admin":
        await shell.rookery.users.setAdmin({ user: user.id, admin: !user.admin });
        break;
      case "disable":
        await shell.rookery.users.setDisabled({ user: user.id, disabled: !user.disabled });
        break;
      case "link": {
        const answer = await shell.rookery.users.createLink({ email: user.email });
        props.onLink({ email: user.email, url: answer.link?.url ?? "" });
        return;
      }
    }
    props.onChanged();
  }, { failed: "The change did not go through" });

  const menu = (user: User) => [
    { id: "link", label: "New sign-in link", icon: "link" as const },
    { type: "separator" as const },
    { id: "admin", label: user.admin ? "Remove admin" : "Make admin", icon: "lock" as const, disabled: user.id === me() },
    { id: "disable", label: user.disabled ? "Let in again" : "Disable", danger: !user.disabled, disabled: user.id === me() },
  ];

  return (
    <Show when={props.users} fallback={<Skeleton width="50%" />}>
      {(users) => (
        <Show
          when={users().length > 0}
          fallback={
            <Card>
              <EmptyState icon="users" title="No users" description="A person gets a user the first time they use a sign-in link.">
                <Button variant="primary" icon="plus" onClick={props.onInvite}>
                  Invite
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Users">
              <For each={users()}>
                {(user) => (
                  <ListRow
                    data-user={user.email}
                    leading={<Avatar name={user.name || user.email} size="sm" />}
                    title={user.name || user.email}
                    secondary={user.name ? user.email : undefined}
                    muted={user.disabled}
                    status={
                      <span class="nc-row nc-row--tight">
                        <Show when={user.id === me()}>
                          <Badge>you</Badge>
                        </Show>
                        <Show when={user.admin}>
                          <Badge tone="accent">admin</Badge>
                        </Show>
                        <Show when={user.disabled}>
                          <Badge tone="danger">disabled</Badge>
                        </Show>
                      </span>
                    }
                    menu={menu(user)}
                    menuLabel={`Actions for ${user.email}`}
                    onMenuSelect={(id) => void change.run(user, id)}
                    persistentActions={
                      <RowActions label={`Actions for ${user.email}`} items={menu(user)} onSelect={(id) => void change.run(user, id)} />
                    }
                  />
                )}
              </For>
            </RowList>
          </Card>
        </Show>
      )}
    </Show>
  );
}

function GroupsTab(props: { groups: readonly Group[] | undefined; onChanged: () => void; onCreate: () => void }): JSX.Element {
  const shell = useShell();
  const [openId, setOpenId] = createSignal<string>();
  const [removing, setRemoving] = createSignal<Group>();
  const open = createMemo(() => props.groups?.find((group) => group.id === openId()));
  const remove = createAction(async () => {
    const group = removing();
    if (group === undefined) return;
    await shell.rookery.groups.delete({ group: group.id });
    props.onChanged();
  }, { done: "Group deleted" });

  return (
    <Show when={props.groups} fallback={<Skeleton width="50%" />}>
      {(groups) => (
        <>
          <Show
            when={groups().length > 0}
            fallback={
              <Card>
                <EmptyState
                  icon="users"
                  title="No groups"
                  description="A grant can name a group, so people are added and removed in one place."
                >
                  <Button variant="primary" icon="plus" onClick={props.onCreate}>
                    New group
                  </Button>
                </EmptyState>
              </Card>
            }
          >
            <Card padding="none">
              <RowList label="Groups">
                <For each={groups()}>
                  {(group) => (
                    <ListRow
                      data-group={group.name}
                      icon="users"
                      title={group.name}
                      secondary={`${group.members.length} member${group.members.length === 1 ? "" : "s"}`}
                      meta={
                        <span class="nc-row nc-row--tight">
                          <For each={group.claims}>{(bound) => <Badge tone="info">{`${bound.claim}=${bound.value}`}</Badge>}</For>
                        </span>
                      }
                      onClick={() => setOpenId(group.id)}
                      menu={[{ id: "delete", label: "Delete group", icon: "trash", danger: true }]}
                      onMenuSelect={() => setRemoving(group)}
                      actions={<IconButton size="sm" icon="trash" label={`Delete ${group.name}`} onClick={() => setRemoving(group)} />}
                    />
                  )}
                </For>
              </RowList>
            </Card>
          </Show>
          <Show when={open()} keyed>
            {(group) => <GroupDialog group={group} onClose={() => setOpenId(undefined)} onChanged={props.onChanged} />}
          </Show>
          <ConfirmDialog
            open={removing() !== undefined}
            title={`Delete ${removing()?.name ?? "the group"}?`}
            description="Its grants go with it. The people in it keep their users."
            confirm="Delete group"
            danger
            action={remove}
            onClose={() => setRemoving(undefined)}
          />
        </>
      )}
    </Show>
  );
}

/** GroupDialog shows a group's members and claims, and changes them. */
function GroupDialog(props: { group: Group; onClose: () => void; onChanged: () => void }): JSX.Element {
  const shell = useShell();
  const [email, setEmail] = createSignal("");
  const [claim, setClaim] = createSignal("groups");
  const [value, setValue] = createSignal("");
  const add = createAction(async () => {
    await shell.rookery.groups.addMember({ group: props.group.id, email: email().trim() });
    setEmail("");
    props.onChanged();
  });
  const removeMember = createAction(async (member: string) => {
    await shell.rookery.groups.removeMember({ group: props.group.id, email: member });
    props.onChanged();
  }, { failed: "The member was not removed" });
  const bind = createAction(async () => {
    await shell.rookery.groups.bindClaim({ group: props.group.id, claim: claim().trim(), value: value().trim() });
    setValue("");
    props.onChanged();
  });

  return (
    <Dialog
      open
      onClose={props.onClose}
      title={props.group.name}
      description="Members are people by email. A claim from the identity provider adds whoever carries it."
      size="lg"
      footer={
        <Button variant="primary" onClick={props.onClose}>
          Done
        </Button>
      }
    >
      <div class="nc-stack">
        <form
          class="shell-form__row shell-form__row--end"
          onSubmit={(event) => {
            event.preventDefault();
            if (email().trim() !== "") void add.run();
          }}
        >
          <TextField
            label="Add a member"
            type="email"
            placeholder="ada@example.test"
            value={email()}
            error={add.error()}
            onInput={(event) => setEmail(event.currentTarget.value)}
          />
          <Button type="submit" loading={add.busy()} disabled={email().trim() === ""}>
            Add
          </Button>
        </form>
        <Show
          when={props.group.members.length > 0}
          fallback={<p class="nc-muted">No members yet.</p>}
        >
          <RowList label="Members" variant="inline">
            <For each={props.group.members}>
              {(member) => (
                <ListRow
                  leading={<Avatar name={member.name || member.email} size="sm" />}
                  title={member.name || member.email}
                  secondary={member.name ? member.email : undefined}
                  actions={
                    <IconButton
                      size="sm"
                      icon="close"
                      label={`Remove ${member.email}`}
                      onClick={() => void removeMember.run(member.email)}
                    />
                  }
                />
              )}
            </For>
          </RowList>
        </Show>
        <form
          class="shell-form__row shell-form__row--end"
          onSubmit={(event) => {
            event.preventDefault();
            if (claim().trim() !== "" && value().trim() !== "") void bind.run();
          }}
        >
          <TextField label="Claim" mono value={claim()} onInput={(event) => setClaim(event.currentTarget.value)} />
          <TextField
            label="Value"
            mono
            placeholder="engineering"
            value={value()}
            error={bind.error()}
            onInput={(event) => setValue(event.currentTarget.value)}
          />
          <Button type="submit" loading={bind.busy()} disabled={claim().trim() === "" || value().trim() === ""}>
            Bind
          </Button>
        </form>
        <Show when={props.group.claims.length > 0}>
          <div class="nc-row nc-row--tight">
            <For each={props.group.claims}>{(bound) => <Badge tone="info">{`${bound.claim}=${bound.value}`}</Badge>}</For>
          </div>
        </Show>
      </div>
    </Dialog>
  );
}

function GrantsTab(props: { users: readonly User[]; groups: readonly Group[]; version: number; onGrant: () => void }): JSX.Element {
  const shell = useShell();
  const [resource, setResource] = createSignal("");
  const grants = createLive(() => shell.rookery.access.listGrants({ resource: resource().trim(), limit: 200 }), {
    key: () => [resource().trim(), props.version],
  });
  const revoke = createAction(async (grant: Grant) => {
    await shell.rookery.access.revoke({ resource: grant.resource, grantee: grant.grantee });
    void grants.refresh().catch(() => undefined);
  }, { done: "Access revoked", failed: "The grant was not revoked" });

  const columns = (): DataColumn<Grant>[] => [
    { id: "resource", header: "Resource", type: "mono", accessor: (grant) => grant.resource },
    { id: "who", header: "Who", accessor: (grant) => granteeName(grant, props.users, props.groups) },
    { id: "level", header: "Level", accessor: (grant) => levelName(grant.level) },
    {
      id: "actions",
      header: "",
      cell: (grant) => (
        <IconButton size="sm" icon="trash" label={`Revoke ${grant.resource}`} onClick={() => void revoke.run(grant)} />
      ),
    },
  ];

  return (
    <div class="nc-stack">
      <TextField
        variant="search"
        aria-label="Resource"
        placeholder="Filter by resource, such as host://forge"
        mono
        value={resource()}
        onInput={(event) => setResource(event.currentTarget.value)}
      />
      <Card padding="none">
        <DataTable
          bare
          label="Grants"
          rows={grants()?.grants ?? []}
          columns={columns()}
          getRowId={(grant) => `${grant.resource}/${granteeName(grant, props.users, props.groups)}`}
          loading={grants() === undefined}
          empty={{
            title: "No grants",
            description: "Admins hold every level everywhere. Everyone else holds what a grant gives them.",
          }}
        />
      </Card>
    </div>
  );
}

function InviteDialog(props: { onClose: () => void; onMade: (made: { email: string; url: string }) => void }): JSX.Element {
  const shell = useShell();
  const [email, setEmail] = createSignal("");
  const make = createAction(async () => {
    const answer = await shell.rookery.users.createLink({ email: email().trim() });
    props.onMade({ email: email().trim(), url: answer.link?.url ?? "" });
  });
  return (
    <FormDialog
      open
      title="Invite someone"
      description="Rookery makes a one-time sign-in link for the address. It sends no mail."
      submit="Make link"
      action={make}
      disabled={!email().includes("@")}
      onClose={props.onClose}
    >
      <TextField
        label="Email"
        type="email"
        placeholder="ada@example.test"
        help="A person without a user gets one when they use the link."
        value={email()}
        onInput={(event) => setEmail(event.currentTarget.value)}
      />
    </FormDialog>
  );
}

function NewGroupDialog(props: { onClose: () => void; onCreated: () => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const create = createAction(async () => {
    await shell.rookery.groups.create({ name: name().trim() });
  }, { done: "Group created" });
  return (
    <FormDialog
      open
      title="New group"
      submit="Create group"
      action={create}
      disabled={name().trim() === ""}
      onClose={props.onClose}
      onDone={props.onCreated}
    >
      <TextField label="Name" placeholder="Platform team" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
    </FormDialog>
  );
}

type Grantee = "user" | "group" | "everyone";

function GrantDialog(props: {
  users: readonly User[];
  groups: readonly Group[];
  onClose: () => void;
  onGranted: () => void;
}): JSX.Element {
  const shell = useShell();
  const space = shell.spaces.current();
  const [resource, setResource] = createSignal(space === "" ? "" : `space://${space}`);
  const [grantee, setGrantee] = createSignal<Grantee>("user");
  const [who, setWho] = createSignal("");
  const [level, setLevel] = createSignal(String(Level.VIEW));
  const grant = createAction(async () => {
    const target =
      grantee() === "user"
        ? { case: "userId" as const, value: who() }
        : grantee() === "group"
          ? { case: "groupId" as const, value: who() }
          : { case: "everyone" as const, value: true };
    await shell.rookery.access.grant({ resource: resource().trim(), grantee: { who: target }, level: Number(level()) as Level });
  }, { done: "Access granted" });
  const complete = () => resource().trim() !== "" && (grantee() === "everyone" || who() !== "");
  return (
    <FormDialog
      open
      title="Grant access"
      submit="Grant"
      action={grant}
      disabled={!complete()}
      onClose={props.onClose}
      onDone={props.onGranted}
    >
      <div class="nc-stack">
        <TextField
          label="Resource"
          mono
          placeholder="space://…"
          help="Such as space://01h…, host://forge or plugin://example."
          value={resource()}
          onInput={(event) => setResource(event.currentTarget.value)}
        />
        <div class="shell-form__row">
          <Select
            label="For"
            value={grantee()}
            options={[
              { value: "user", label: "A user" },
              { value: "group", label: "A group" },
              { value: "everyone", label: "Everyone signed in" },
            ]}
            onChange={(value) => {
              setGrantee(value as Grantee);
              setWho("");
            }}
          />
          <Show when={grantee() === "user"}>
            <Select
              label="User"
              placeholder="Choose a user"
              value={who()}
              options={props.users.map((user) => ({ value: user.id, label: user.email || user.name || user.id }))}
              onChange={setWho}
            />
          </Show>
          <Show when={grantee() === "group"}>
            <Select
              label="Group"
              placeholder="Choose a group"
              value={who()}
              options={props.groups.map((group) => ({ value: group.id, label: group.name }))}
              onChange={setWho}
            />
          </Show>
        </div>
        <Select label="Level" value={level()} options={LEVELS} onChange={setLevel} />
      </div>
    </FormDialog>
  );
}

/** granteeName is who a grant is for, in words. */
function granteeName(grant: Grant, users: readonly User[], groups: readonly Group[]): string {
  const who = grant.grantee?.who;
  switch (who?.case) {
    case "userId":
      return users.find((user) => user.id === who.value)?.email ?? who.value;
    case "groupId":
      return groups.find((group) => group.id === who.value)?.name ?? who.value;
    case "everyone":
      return "everyone";
    default:
      return "—";
  }
}

/** levelName is a level in words. */
function levelName(level: Level): string {
  switch (level) {
    case Level.VIEW:
      return "View";
    case Level.USE:
      return "Use";
    case Level.MANAGE:
      return "Manage";
    case Level.ADMIN:
      return "Admin";
    default:
      return "—";
  }
}
