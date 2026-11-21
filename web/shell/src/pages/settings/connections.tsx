// Connections: credentials of a type the core or a plugin declares, such as a
// container registry login. A host's profile names them, and a type that can
// be tested says whether a connection works.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import type { Connection, ConnectionType } from "@rookery/proto";
import { Badge, Button, Callout, Card, EmptyState, ListRow, RowList, Select, Skeleton, TextField, toast } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { useShell } from "../../shell.tsx";

/** ConnectionsSection lists the connections and makes, tests and removes them. */
export function ConnectionsSection(props: { version: number; adding: boolean; onAdd: () => void; onCloseAdd: () => void }): JSX.Element {
  const shell = useShell();
  const types = createLive(() => shell.rookery.connections.listTypes({}), { key: () => props.version });
  const connections = createLive(() => shell.rookery.connections.list({}), { key: () => props.version });
  const [removing, setRemoving] = createSignal<Connection>();
  const refresh = () => void connections.refresh().catch(() => undefined);
  const typeOf = (id: string) => (types()?.types ?? []).find((one) => one.id === id);

  const test = createAction(async (connection: Connection) => {
    const answer = await shell.rookery.connections.test({ connection: connection.id });
    toast({
      title: answer.ok ? `${connection.name} works` : `${connection.name} does not work`,
      description: answer.message || undefined,
      tone: answer.ok ? "success" : "danger",
    });
  }, { failed: "The test did not run" });
  const remove = createAction(async () => {
    const connection = removing();
    if (connection !== undefined) await shell.rookery.connections.delete({ connection: connection.id });
    refresh();
  }, { done: "Connection removed" });

  const menu = (connection: Connection) => [
    { id: "test", label: "Test", icon: "check" as const, disabled: typeOf(connection.type)?.test !== true },
    { type: "separator" as const },
    { id: "remove", label: "Remove", icon: "trash" as const, danger: true },
  ];
  const choose = (connection: Connection, id: string) => (id === "test" ? void test.run(connection) : setRemoving(connection));

  return (
    <>
      <Show when={connections.error()}>
        {(failure) => <Callout tone="danger">The connections could not be read. {errorMessage(failure())}</Callout>}
      </Show>
      <Show when={connections()} fallback={<Skeleton width="50%" />}>
        {(answer) => (
          <Show
            when={answer().connections.length > 0}
            fallback={
              <Card>
                <EmptyState
                  icon="link"
                  title="No connections"
                  description="A connection is a login a host or a plugin uses, such as a container registry's. It is kept sealed."
                >
                  <Button variant="primary" icon="plus" onClick={props.onAdd}>
                    Add connection
                  </Button>
                </EmptyState>
              </Card>
            }
          >
            <Card padding="none">
              <RowList label="Connections">
                <For each={answer().connections} keyed={(connection) => connection.id}>
                  {(connection) => (
                    <ListRow
                      icon="link"
                      title={<span class="nc-mono">{connection().name}</span>}
                      label={connection().name}
                      secondary={typeOf(connection().type)?.label ?? connection().type}
                      meta={<Badge tone="outline">{connection().type}</Badge>}
                      menu={menu(connection())}
                      onMenuSelect={(id) => choose(connection(), id)}
                      persistentActions={
                        <RowActions label={`Actions for ${connection().name}`} items={menu(connection())} onSelect={(id) => choose(connection(), id)} />
                      }
                    />
                  )}
                </For>
              </RowList>
            </Card>
          </Show>
        )}
      </Show>
      <Show when={props.adding}>
        <AddConnection types={types()?.types ?? []} onClose={props.onCloseAdd} onAdded={refresh} />
      </Show>
      <ConfirmDialog
        open={removing() !== undefined}
        title={`Remove ${removing()?.name ?? "the connection"}?`}
        description="Hosts whose profile names it lose the login on their next apply."
        confirm="Remove connection"
        danger
        action={remove}
        onClose={() => setRemoving(undefined)}
      />
    </>
  );
}

function AddConnection(props: { types: readonly ConnectionType[]; onClose: () => void; onAdded: () => void }): JSX.Element {
  const shell = useShell();
  const [type, setType] = createSignal("");
  const [name, setName] = createSignal("");
  const [values, setValues] = createSignal<Record<string, string>>({});
  const chosen = () => props.types.find((one) => one.id === type());
  const missing = () => (chosen()?.fields ?? []).some((field) => field.required && (values()[field.name] ?? "") === "");
  const create = createAction(async () => {
    await shell.rookery.connections.create({ type: type(), name: name().trim(), valuesCbor: cbor.encode(values()) });
  }, { done: "Connection added" });
  return (
    <FormDialog
      open
      title="Add connection"
      submit="Add connection"
      action={create}
      disabled={type() === "" || name().trim() === "" || missing()}
      onClose={props.onClose}
      onDone={props.onAdded}
    >
      <div class="nc-stack">
        <Select
          label="Type"
          placeholder="Choose a type"
          value={type()}
          options={props.types.map((one) => ({
            value: one.id,
            label: one.label,
            description: one.plugin === "" ? undefined : `From ${one.plugin}`,
          }))}
          help={props.types.length === 0 ? "No plugin declares a type of connection yet." : undefined}
          onChange={(next) => {
            setType(next);
            setValues({});
          }}
        />
        <TextField label="Name" mono placeholder="lab-registry" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
        <Show when={chosen()}>
          {(one) => (
            <For each={one().fields}>
              {(field) => (
                <TextField
                  label={field.label || field.name}
                  type={field.secret ? "password" : "text"}
                  autocomplete={field.secret ? "new-password" : undefined}
                  help={field.required ? undefined : "Optional."}
                  value={values()[field.name] ?? ""}
                  onInput={(event) => {
                    const value = event.currentTarget.value;
                    setValues((current) => ({ ...current, [field.name]: value }));
                  }}
                />
              )}
            </For>
          )}
        </Show>
      </div>
    </FormDialog>
  );
}
