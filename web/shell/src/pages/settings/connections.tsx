// Connections: credentials of a type the core or a plugin declares, such as a
// container registry login.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { createQuery } from "@rookery/client/solid";
import type { Connection, ConnectionType } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  DataTable,
  type DataColumn,
  Select,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";

/** ConnectionsSection lists the connections and makes, tests and removes them. */
export function ConnectionsSection(): JSX.Element {
  const shell = useShell();
  const types = createQuery(() => shell.rookery.connections.listTypes({}));
  const connections = createQuery(() => shell.rookery.connections.list({}));

  const [type, setType] = createSignal("");
  const [name, setName] = createSignal("");
  const [values, setValues] = createSignal<Record<string, string>>({});
  const [error, setError] = createSignal("");
  const [tested, setTested] = createSignal("");

  const chosenType = (): ConnectionType | undefined =>
    (types()?.types ?? []).find((candidate) => candidate.id === type());
  const setValue = (field: string, value: string) => setValues((current) => ({ ...current, [field]: value }));

  const create = async () => {
    setError("");
    if (type() === "" || name().trim() === "") return;
    try {
      await shell.rookery.connections.create({
        type: type(),
        name: name().trim(),
        valuesCbor: cbor.encode(values()),
      });
      setName("");
      setValues({});
      void connections.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const columns = (): DataColumn<Connection>[] => [
    { id: "name", header: "Name", type: "mono", accessor: connection => connection.name },
    {
      id: "type",
      header: "Type",
      accessor: connection => connection.type,
      cell: connection => <Badge tone="outline">{connection.type}</Badge>,
    },
    {
      id: "actions",
      header: "",
      cell: connection => (
        <div class="nc-row nc-row--tight">
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setTested("");
              void shell.rookery.connections
                .test({ connection: connection.id })
                .then((answer) =>
                  setTested(
                    `${connection.name}: ${answer.ok ? "works" : "does not work"}${answer.message === "" ? "" : ` — ${answer.message}`}`,
                  ),
                )
                .catch((failure: unknown) => setTested(failure instanceof Error ? failure.message : String(failure)));
            }}
          >
            Test
          </Button>
          <Button
            size="sm"
            variant="danger"
            onClick={() => {
              void shell.rookery.connections.delete({ connection: connection.id }).then(() => connections.refresh());
            }}
          >
            Remove
          </Button>
        </div>
      ),
    },
  ];

  return (
    <Card title="Connections" padding="none">
      <div class="shell-pad nc-stack">
        <div class="nc-row">
          <Select
            label="Type"
            value={type()}
            options={[
              { value: "", label: "Choose a type" },
              ...(types()?.types ?? []).map((connectionType) => ({
                value: connectionType.id,
                label: connectionType.plugin === "" ? connectionType.label : `${connectionType.label} (${connectionType.plugin})`,
              })),
            ]}
            onChange={setType}
          />
          <TextField
            label="Name"
            id="connection-name"
            value={name()}
            placeholder="lab-registry"
            onInput={event => setName(event.currentTarget.value)}
          />
        </div>
        <Show when={chosenType()}>
          {connectionType => (
            <div class="nc-row">
              <For each={connectionType().fields}>
                {field => (
                  <TextField
                    label={field.label || field.name}
                    id={`connection-field-${field.name}`}
                    type={field.secret ? "password" : "text"}
                    autocomplete={field.secret ? "new-password" : undefined}
                    value={values()[field.name] ?? ""}
                    onInput={event => setValue(field.name, event.currentTarget.value)}
                  />
                )}
              </For>
            </div>
          )}
        </Show>
        <div class="nc-row">
          <Button variant="primary" size="sm" onClick={() => void create()}>
            Create
          </Button>
        </div>
        <Show when={error() !== ""}>
          <Callout tone="danger">{error()}</Callout>
        </Show>
        <Show when={tested() !== ""}>
          <Callout tone="info">{tested()}</Callout>
        </Show>
      </div>
      <DataTable
        bare
        label="Connections"
        rows={connections()?.connections ?? []}
        columns={columns()}
        getRowId={connection => connection.id}
        loading={connections.pending() && connections() === undefined}
        empty={{
          title: "No connections",
          description: "A connection is a login a host or a plugin uses, kept sealed.",
        }}
      />
    </Card>
  );
}
