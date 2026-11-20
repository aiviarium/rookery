// People, groups and grants: who may sign in, what gathers them, and what they
// hold.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { Level } from "@rookery/proto";
import type { Grant } from "@rookery/proto";

import { useShell } from "../../shell.tsx";
import {
  Badge,
  Button,
  Callout,
  Card,
  Code,
  Empty,
  Field,
  Input,
  Page,
  Select,
  Spinner,
  Table,
} from "../../ui.tsx";

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

  return (
    <Page title="People" description="Who may sign in, what gathers them, and what they hold.">
      <Show when={link() !== ""}>
        <Callout tone="info">
          <div class="nc-callout__body nc-stack">
            <span>A one-time sign-in link. It works once and lasts a day.</span>
            <Code>{link()}</Code>
            <Button variant="ghost" size="sm" onClick={() => setLink("")}>
              Done
            </Button>
          </div>
        </Callout>
      </Show>

      <Card
        title="Sign-in link"
        actions={
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
          <Field label="Email" hint="A person without a user gets one when the link is used.">
            <Input id="link-email" type="email" value={linkEmail()} onInput={setLinkEmail} placeholder="ada@example.test" />
          </Field>
          <Show when={linkError() !== ""}>
            <Callout tone="danger">{linkError()}</Callout>
          </Show>
        </div>
      </Card>

      <Card title="Users">
        <Show when={users() !== undefined} fallback={<Spinner />}>
          <Table columns={["Email", "Name", "Admin", "Signs in", ""]}>
            <For each={users()?.users ?? []}>
              {person => (
                <tr>
                  <td>{person.email}</td>
                  <td>{person.name}</td>
                  <td>
                    <Show when={person.admin} fallback={<span class="nc-meta">no</span>}>
                      <Badge tone="accent">admin</Badge>
                    </Show>
                  </td>
                  <td>
                    <Show when={!person.disabled} fallback={<Badge tone="danger">disabled</Badge>}>
                      <Badge tone="success">yes</Badge>
                    </Show>
                  </td>
                  <td>
                    <div class="nc-row nc-row--tight">
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          void shell.rookery.users.setAdmin({ user: person.id, admin: !person.admin }).then(refreshAll);
                        }}
                      >
                        {person.admin ? "Remove admin" : "Make admin"}
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          void shell.rookery.users.setDisabled({ user: person.id, disabled: !person.disabled }).then(refreshAll);
                        }}
                      >
                        {person.disabled ? "Let in" : "Disable"}
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          void makeLink(person.email);
                        }}
                      >
                        Link
                      </Button>
                    </div>
                  </td>
                </tr>
              )}
            </For>
          </Table>
        </Show>
      </Card>

      <Card
        title="Groups"
        actions={
          <div class="nc-row nc-row--tight">
            <Input id="new-group" value={newGroup()} placeholder="Team name" onInput={setNewGroup} />
            <Button size="sm" onClick={() => void addGroup()}>
              Create
            </Button>
          </div>
        }
      >
        <Show when={groups() !== undefined} fallback={<Spinner />}>
          <Show
            when={(groups()?.groups.length ?? 0) > 0}
            fallback={<Empty title="No groups" description="A group is what a grant names, so people can be added and removed in one place." />}
          >
            <div class="nc-stack">
              <For each={groups()?.groups ?? []}>
                {group => (
                  <div class="nc-card nc-pad nc-stack">
                    <div class="nc-row" style={{ "justify-content": "space-between" }}>
                      <strong>{group.name}</strong>
                      <div class="nc-row nc-row--tight">
                        <Badge tone="outline">{group.members.length} members</Badge>
                        <For each={group.claims}>{(bound) => <Badge tone="info">{bound.claim}={bound.value}</Badge>}</For>
                        <Button
                          size="sm"
                          variant="danger"
                          onClick={() => {
                            void shell.rookery.groups.delete({ group: group.id }).then(refreshAll);
                          }}
                        >
                          Delete
                        </Button>
                      </div>
                    </div>
                    <div class="nc-row nc-wrap" style={{ gap: "var(--space-2)" }}>
                      <For each={group.members}>{(member) => <Badge tone="outline">{member.email || member.name}</Badge>}</For>
                      <Show when={group.members.length === 0}>
                        <span class="nc-meta">No members yet.</span>
                      </Show>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </Show>
        <div class="nc-stack" style={{ "margin-top": "var(--space-3)" }}>
          <div class="nc-row">
            <Select
              value={groupID()}
              options={[{ value: "", label: "Choose a group" }, ...groupOptions()]}
              onChange={setGroupID}
            />
            <Input id="member-email" type="email" value={memberEmail()} placeholder="ada@example.test" onInput={setMemberEmail} />
            <Button size="sm" onClick={() => void addMember()}>
              Add member
            </Button>
          </div>
          <div class="nc-row">
            <Input id="claim" value={claim()} placeholder="Claim" onInput={setClaim} />
            <Input id="claim-value" value={claimValue()} placeholder="Value" onInput={setClaimValue} />
            <Button size="sm" onClick={() => void bindClaim()}>
              Bind claim
            </Button>
          </div>
        </div>
      </Card>

      <Card
        title="Grants"
        actions={
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void grants.refresh();
            }}
          >
            Refresh
          </Button>
        }
      >
        <div class="nc-stack">
          <Field label="Resource" hint="For example space://01h…, host://forge, plugin://example.">
            <Input id="grant-resource" value={resource()} placeholder="space://…" onInput={setResource} />
          </Field>
          <div class="nc-row">
            <Select
              value={grantee()}
              options={[
                { value: "user", label: "A user" },
                { value: "group", label: "A group" },
                { value: "everyone", label: "Everyone" },
              ]}
              onChange={(value) => setGrantee(value as "user" | "group" | "everyone")}
            />
            <Show when={grantee() === "user"}>
              <Select value={userID()} options={[{ value: "", label: "Choose a user" }, ...userOptions()]} onChange={setUserID} />
            </Show>
            <Show when={grantee() === "group"}>
              <Select value={groupID()} options={[{ value: "", label: "Choose a group" }, ...groupOptions()]} onChange={setGroupID} />
            </Show>
            <Select value={level()} options={LEVELS} onChange={setLevel} />
            <Button size="sm" variant="primary" onClick={() => void grant()}>
              Grant
            </Button>
          </div>
          <Show when={grantError() !== ""}>
            <Callout tone="danger">{grantError()}</Callout>
          </Show>

          <Show when={grants() !== undefined} fallback={<Spinner />}>
            <Show
              when={(grants()?.grants.length ?? 0) > 0}
              fallback={<Empty title="No grants here" description="An admin holds every level everywhere and needs none." />}
            >
              <Table columns={["Resource", "Who", "Level", ""]}>
                <For each={grants()?.grants ?? []}>
                  {(grant: Grant) => (
                    <tr>
                      <td class="nc-mono">{grant.resource}</td>
                      <td>{granteeName(grant, users()?.users ?? [], groups()?.groups ?? [])}</td>
                      <td>{levelName(grant.level)}</td>
                      <td>
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
                      </td>
                    </tr>
                  )}
                </For>
              </Table>
            </Show>
          </Show>
        </div>
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
