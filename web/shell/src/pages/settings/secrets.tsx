// Secrets: the deployment's sealed values, by name and scope. A value is never
// read back, so the page shows names only; setting a name again replaces it.
import { For, Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Secret } from "@rookery/proto";
import { Badge, Button, Callout, Card, EmptyState, ListRow, RowList, Select, Skeleton, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { useShell } from "../../shell.tsx";
import { dateOf, relativeTime } from "../infra/format.ts";

/** SCOPES are the places a secret can belong to. */
const SCOPES = [
  { value: "global", label: "Deployment", description: "Anything that asks for it by name." },
  { value: "plugin", label: "A plugin", description: "Only that plugin." },
  { value: "provisioner", label: "A provisioner", description: "Only that provisioner." },
  { value: "host", label: "A host", description: "Only that host's applies." },
];

/** SecretsSection lists the secrets and sets, renames and removes them. */
export function SecretsSection(props: { version: number; adding: boolean; onAdd: () => void; onCloseAdd: () => void }): JSX.Element {
  const shell = useShell();
  const secrets = createLive(() => shell.rookery.secrets.list({}), { key: () => props.version });
  const [replacing, setReplacing] = createSignal<Secret>();
  const [renaming, setRenaming] = createSignal<Secret>();
  const [removing, setRemoving] = createSignal<Secret>();
  const refresh = () => void secrets.refresh().catch(() => undefined);
  const remove = createAction(async () => {
    const secret = removing();
    if (secret !== undefined) await shell.rookery.secrets.delete({ name: secret.name, scope: secret.scope });
    refresh();
  }, { done: "Secret removed" });

  const menu = [
    { id: "replace", label: "Replace the value", icon: "lock" as const },
    { id: "rename", label: "Rename", icon: "edit" as const },
    { type: "separator" as const },
    { id: "remove", label: "Remove", icon: "trash" as const, danger: true },
  ];
  const choose = (secret: Secret, id: string) => {
    if (id === "replace") setReplacing(secret);
    if (id === "rename") setRenaming(secret);
    if (id === "remove") setRemoving(secret);
  };

  return (
    <>
      <Show when={secrets.error()}>
        {(failure) => <Callout tone="danger">The secrets could not be read. {errorMessage(failure())}</Callout>}
      </Show>
      <Show when={secrets()} fallback={<Skeleton width="50%" />}>
        {(answer) => (
          <Show
            when={answer().secrets.length > 0}
            fallback={
              <Card>
                <EmptyState
                  icon="lock"
                  title="No secrets"
                  description="A secret is sealed on the server and reaches only what its scope names."
                >
                  <Button variant="primary" icon="plus" onClick={props.onAdd}>
                    Add secret
                  </Button>
                </EmptyState>
              </Card>
            }
          >
            <Card padding="none">
              <RowList label="Secrets">
                <For each={answer().secrets} keyed={(secret) => `${secret.scope}/${secret.name}`}>
                  {(secret) => (
                    <ListRow
                      icon="lock"
                      title={<span class="nc-mono">{secret().name}</span>}
                      label={secret().name}
                      meta={
                        <span class="nc-row nc-row--tight">
                          <Badge tone="outline">{secret().scope}</Badge>
                          <span>{relativeTime(dateOf(secret().updatedAt ?? secret().createdAt))}</span>
                        </span>
                      }
                      menu={menu}
                      onMenuSelect={(id) => choose(secret(), id)}
                      persistentActions={<RowActions label={`Actions for ${secret().name}`} items={menu} onSelect={(id) => choose(secret(), id)} />}
                    />
                  )}
                </For>
              </RowList>
            </Card>
          </Show>
        )}
      </Show>
      <Show when={props.adding}>
        <SetSecret onClose={props.onCloseAdd} onSet={refresh} />
      </Show>
      <Show when={replacing()} keyed>
        {(secret) => <SetSecret secret={secret} onClose={() => setReplacing(undefined)} onSet={refresh} />}
      </Show>
      <Show when={renaming()} keyed>
        {(secret) => <RenameSecret secret={secret} onClose={() => setRenaming(undefined)} onRenamed={refresh} />}
      </Show>
      <ConfirmDialog
        open={removing() !== undefined}
        title={`Remove ${removing()?.name ?? "the secret"}?`}
        description="Whatever reads it fails until it is set again."
        confirm="Remove secret"
        danger
        action={remove}
        onClose={() => setRemoving(undefined)}
      />
    </>
  );
}

/** SetSecret adds a secret, or replaces the value of the one it is given. */
function SetSecret(props: { secret?: Secret; onClose: () => void; onSet: () => void }): JSX.Element {
  const shell = useShell();
  const existing = untrack(() => props.secret);
  const [name, setName] = createSignal(existing?.name ?? "");
  const [kind, setKind] = createSignal(existing === undefined ? "global" : existing.scope.split("/")[0]!);
  const [subject, setSubject] = createSignal(existing?.scope.split("/").slice(1).join("/") ?? "");
  const [value, setValue] = createSignal("");
  const scope = () => (kind() === "global" ? "global" : `${kind()}/${subject().trim()}`);
  const set = createAction(async () => {
    await shell.rookery.secrets.set({ name: name().trim(), scope: scope(), value: new TextEncoder().encode(value()) });
  }, { done: existing === undefined ? "Secret added" : "Secret replaced" });
  return (
    <FormDialog
      open
      title={existing === undefined ? "Add secret" : `Replace ${existing.name}`}
      description="The value is sealed on the server and never shown again."
      submit={existing === undefined ? "Add secret" : "Replace"}
      action={set}
      disabled={name().trim() === "" || value() === "" || (kind() !== "global" && subject().trim() === "")}
      onClose={props.onClose}
      onDone={props.onSet}
    >
      <div class="nc-stack">
        <Show when={existing === undefined}>
          <TextField label="Name" mono placeholder="github_token" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
          <div class="shell-form__row">
            <Select label="Scope" value={kind()} options={SCOPES} onChange={setKind} />
            <Show when={kind() !== "global"}>
              <TextField
                label={kind() === "host" ? "Host" : kind() === "plugin" ? "Plugin" : "Provisioner"}
                mono
                placeholder={kind() === "host" ? "forge" : "example"}
                value={subject()}
                onInput={(event) => setSubject(event.currentTarget.value)}
              />
            </Show>
          </div>
        </Show>
        <TextField
          label="Value"
          type="password"
          autocomplete="new-password"
          value={value()}
          onInput={(event) => setValue(event.currentTarget.value)}
        />
      </div>
    </FormDialog>
  );
}

function RenameSecret(props: { secret: Secret; onClose: () => void; onRenamed: () => void }): JSX.Element {
  const shell = useShell();
  const secret = untrack(() => props.secret);
  const [name, setName] = createSignal(secret.name);
  const rename = createAction(async () => {
    await shell.rookery.secrets.rename({ name: secret.name, scope: secret.scope, newName: name().trim() });
  });
  return (
    <FormDialog
      open
      title={`Rename ${secret.name}`}
      submit="Rename"
      action={rename}
      disabled={name().trim() === "" || name().trim() === secret.name}
      onClose={props.onClose}
      onDone={props.onRenamed}
    >
      <TextField label="Name" mono value={name()} onInput={(event) => setName(event.currentTarget.value)} />
    </FormDialog>
  );
}
