// People, groups and grants: who may sign in, what gathers them, and what they
// hold.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { Level } from "@rookery/proto";
import type { Grant, Group, User } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  DataTable,
  type DataColumn,
  EmptyState,
  IconButton,
  ListRow,
  RowList,
  Select,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

/** LEVELS are the levels a grant gives, in order. */
const LEVELS = [
  { value: String(Level.VIEW), label: "view" },
  { value: String(Level.USE), label: "use" },
  { value: String(Level.MANAGE), label: "manage" },
];

/** PeoplePage is the people, their groups and the grants on a resource. */
export function PeoplePage(): JSX.Element {
  const shell = useShell();
  const users = createQuery(() => shell.rookery.users.list({ limit: 500 }));
  const groups = createQuery(() => shell.rookery.groups.list({ limit: 500 }));

  // The sign-in link the admin just made, if any.
  const [link, setLink] = createSignal("");
  const [linkEmail, setLinkEmail] = createSignal("");
  const [linkError, setLinkError] = createSignal("");

  const [newGroup, setNewGroup] = createSignal("");
  const [memberEmail, setMemberEmail] = createSignal("");
  const [claim, setClaim] = createSignal("groups");
  const [claimValue, setClaimValue] = createSignal("");

  const [resource, setResource] = createSignal(shell.spaces.current() === "" ? "" : `space://${shell.spaces.current()}`);
  const [grantee, setGrantee] = createSignal<"user" | "group" | "everyone">("user");
  const [userID, setUserID] = createSignal("");
  const [groupID, setGroupID] = createSignal("");
  const [level, setLevel] = createSignal(String(Level.VIEW));
  const [grantError, setGrantError] = createSignal("");
  const grants = createQuery(() =>
    shell.rookery.access.listGrants({ resource: resource(), limit: 200 }),
  );

  const refreshAll = () => {
    void users.refresh().catch(() => undefined);
    void groups.refresh().catch(() => undefined);
    void grants.refresh().catch(() => undefined);
  };

  const makeLink = async (email: string) => {
    setLinkError("");
    try {
      const answer = await shell.rookery.users.createLink({ email });
      setLink(answer.link?.url ?? "");
    } catch (failure) {
      setLinkError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const addGroup = async () => {
    if (newGroup().trim() === "") return;
    await shell.rookery.groups.create({ name: newGroup().trim() });
    setNewGroup("");
    void groups.refresh();
  };

  const addMember = async () => {
    if (groupID() === "" || memberEmail().trim() === "") return;
    await shell.rookery.groups.addMember({ group: groupID(), email: memberEmail().trim() });
    setMemberEmail("");
    void groups.refresh();
  };

  const bindClaim = async () => {
    if (groupID() === "" || claimValue().trim() === "") return;
    await shell.rookery.groups.bindClaim({ group: groupID(), claim: claim().trim(), value: claimValue().trim() });
    setClaimValue("");
    void groups.refresh();
  };

  const grant = async () => {
    setGrantError("");
    const who =
      grantee() === "user"
        ? { case: "userId" as const, value: userID() }
        : grantee() === "group"
          ? { case: "groupId" as const, value: groupID() }
          : { case: "everyone" as const, value: true };
    try {
      await shell.rookery.access.grant({ resource: resource().trim(), grantee: { who }, level: Number(level()) as Level });
      void grants.refresh();
    } catch (failure) {
      setGrantError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const userOptions = () => (users()?.users ?? []).map((user) => ({ value: user.id, label: user.email || user.name || user.id }));
  const groupOptions = () => (groups()?.groups ?? []).map((group) => ({ value: group.id, label: group.name }));

  const userColumns = (): DataColumn<User>[] => [
    { id: "email", header: "Email", accessor: user => user.email },
    { id: "name", header: "Name", accessor: user => user.name },
    {
      id: "admin",
      header: "Admin",
      accessor: user => (user.admin ? "admin" : "no"),
      cell: user => (
        <Show when={user.admin} fallback={<span class="nc-meta">no</span>}>
          <Badge tone="accent">admin</Badge>
        </Show>
      ),
    },
    {
      id: "signs-in",
      header: "Signs in",
      accessor: user => (user.disabled ? "disabled" : "yes"),
      cell: user => (
        <Show when={!user.disabled} fallback={<Badge tone="danger">disabled</Badge>}>
          <Badge tone="success">yes</Badge>
        </Show>
      ),
    },
    {
      id: "actions",
      header: "",
      cell: user => (
        <div class="nc-row nc-row--tight">
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void shell.rookery.users.setAdmin({ user: user.id, admin: !user.admin }).then(refreshAll);
            }}
          >
            {user.admin ? "Remove admin" : "Make admin"}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void shell.rookery.users.setDisabled({ user: user.id, disabled: !user.disabled }).then(refreshAll);
            }}
          >
            {user.disabled ? "Let in" : "Disable"}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void makeLink(user.email);
            }}
          >
            Link
          </Button>
        </div>
      ),
    },
  ];

  const grantColumns = (): DataColumn<Grant>[] => [
    { id: "resource", header: "Resource", type: "mono", accessor: grant => grant.resource },
    {
      id: "who",
      header: "Who",
      accessor: grant => granteeName(grant, users()?.users ?? [], groups()?.groups ?? []),
    },
    {
      id: "level",
      header: "Level",
      accessor: grant => levelName(grant.level),
    },
    {
      id: "actions",
      header: "",
      cell: grant => (
        <Button
          size="sm"
          variant="danger"
          onClick={() => {
            void shell.rookery.access
              .revoke({ resource: grant.resource, grantee: grant.grantee })
              .then(() => grants.refresh());
          }}
        >
          Revoke
        </Button>
      ),
    },
  ];

  return (
    <Page title="People" description="Who may sign in, what gathers them, and what they hold.">
      <Show when={link() !== ""}>
        <Callout
          tone="info"
          action={
            <Button variant="ghost" size="sm" onClick={() => setLink("")}>
              Done
            </Button>
          }
        >
          A one-time sign-in link. It works once and lasts a day. <code class="nc-code">{link()}</code>
        </Callout>
      </Show>

      <Card
        title="Sign-in link"
        action={
          <Button
            variant="primary"
            size="sm"
            onClick={() => {
              void makeLink(linkEmail());
            }}
          >
            Make a link
          </Button>
        }
      >
        <div class="nc-stack">
          <TextField
            label="Email"
            help="A person without a user gets one when the link is used."
            id="link-email"
            type="email"
            value={linkEmail()}
            placeholder="ada@example.test"
            onInput={event => setLinkEmail(event.currentTarget.value)}
          />
          <Show when={linkError() !== ""}>
            <Callout tone="danger">{linkError()}</Callout>
          </Show>
        </div>
      </Card>

      <Card title="Users" count={users()?.users.length} padding="none">
        <DataTable
          bare
          label="Users"
          rows={users()?.users ?? []}
          columns={userColumns()}
          getRowId={user => user.id}
          loading={users.pending() && users() === undefined}
          empty={{ title: "No users", description: "A person gets a user when they use a sign-in link." }}
        />
      </Card>

      <Card
        title="Groups"
        action={
          <div class="nc-row nc-row--tight">
            <TextField
              aria-label="A new group's name"
              id="new-group"
              value={newGroup()}
              placeholder="Team name"
              onInput={event => setNewGroup(event.currentTarget.value)}
            />
            <Button size="sm" onClick={() => void addGroup()}>
              Create
            </Button>
          </div>
        }
      >
        <Show
          when={(groups()?.groups.length ?? 0) > 0}
          fallback={
            <EmptyState
              size="sm"
              title="No groups"
              description="A group is what a grant names, so people can be added and removed in one place."
            />
          }
        >
          <RowList label="Groups" variant="inline">
            <For each={groups()?.groups ?? []}>
              {(group: Group) => (
                <ListRow
                  title={group.name}
                  secondary={`${group.members.length} member${group.members.length === 1 ? "" : "s"}`}
                  meta={
                    <span class="nc-row nc-row--tight">
                      <For each={group.claims}>
                        {(bound) => <Badge tone="info">{bound.claim}={bound.value}</Badge>}
                      </For>
                    </span>
                  }
                  actions={
                    <IconButton
                      size="sm"
                      label={`Delete ${group.name}`}
                      icon="trash"
                      onClick={() => {
                        void shell.rookery.groups.delete({ group: group.id }).then(refreshAll);
                      }}
                    />
                  }
                />
              )}
            </For>
          </RowList>
        </Show>
        <div class="nc-stack" style={{ "margin-top": "var(--space-3)" }}>
          <div class="nc-row">
            <Select
              aria-label="The group"
              value={groupID()}
              options={[{ value: "", label: "Choose a group" }, ...groupOptions()]}
              onChange={setGroupID}
            />
            <TextField
              aria-label="A person's email"
              id="member-email"
              type="email"
              value={memberEmail()}
              placeholder="ada@example.test"
              onInput={event => setMemberEmail(event.currentTarget.value)}
            />
            <Button size="sm" onClick={() => void addMember()}>
              Add member
            </Button>
          </div>
          <div class="nc-row">
            <TextField aria-label="The claim" id="claim" value={claim()} placeholder="Claim" onInput={event => setClaim(event.currentTarget.value)} />
            <TextField
              aria-label="The claim's value"
              id="claim-value"
              value={claimValue()}
              placeholder="Value"
              onInput={event => setClaimValue(event.currentTarget.value)}
            />
            <Button size="sm" onClick={() => void bindClaim()}>
              Bind claim
            </Button>
          </div>
        </div>
      </Card>

      <Card
        title="Grants"
        action={
          <Button
            size="sm"
            variant="ghost"
            icon="refresh-cw"
            onClick={() => {
              void grants.refresh();
            }}
          >
            Refresh
          </Button>
        }
        padding="none"
      >
        <div class="shell-pad nc-stack">
          <TextField
            label="Resource"
            help="For example space://01h…, host://forge, plugin://example."
            id="grant-resource"
            value={resource()}
            placeholder="space://…"
            onInput={event => setResource(event.currentTarget.value)}
          />
          <div class="nc-row">
            <Select
              aria-label="Who the grant is for"
              value={grantee()}
              options={[
                { value: "user", label: "A user" },
                { value: "group", label: "A group" },
                { value: "everyone", label: "Everyone" },
              ]}
              onChange={(value) => setGrantee(value as "user" | "group" | "everyone")}
            />
            <Show when={grantee() === "user"}>
              <Select aria-label="The user" value={userID()} options={[{ value: "", label: "Choose a user" }, ...userOptions()]} onChange={setUserID} />
            </Show>
            <Show when={grantee() === "group"}>
              <Select aria-label="The group" value={groupID()} options={[{ value: "", label: "Choose a group" }, ...groupOptions()]} onChange={setGroupID} />
            </Show>
            <Select aria-label="The level" value={level()} options={LEVELS} onChange={setLevel} />
            <Button size="sm" variant="primary" onClick={() => void grant()}>
              Grant
            </Button>
          </div>
          <Show when={grantError() !== ""}>
            <Callout tone="danger">{grantError()}</Callout>
          </Show>
        </div>
        <DataTable
          bare
          label="Grants"
          rows={grants()?.grants ?? []}
          columns={grantColumns()}
          getRowId={grant => `${grant.resource}/${granteeName(grant, users()?.users ?? [], groups()?.groups ?? [])}`}
          loading={grants.pending() && grants() === undefined}
          empty={{ title: "No grants here", description: "An admin holds every level everywhere and needs none." }}
        />
      </Card>
    </Page>
  );
}

/** granteeName is who a grant is for, in words. */
function granteeName(
  grant: Grant,
  users: readonly { id: string; email: string; name: string }[],
  groups: readonly { id: string; name: string }[],
): string {
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
      return "view";
    case Level.USE:
      return "use";
    case Level.MANAGE:
      return "manage";
    case Level.ADMIN:
      return "admin";
    default:
      return "—";
  }
}
