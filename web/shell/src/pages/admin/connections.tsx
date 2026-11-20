// Connections: credentials of a type the core or a plugin declares, such as a
// container registry login.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { createQuery } from "@rookery/client/solid";
import type { ConnectionType } from "@rookery/proto";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Empty, Field, Input, Select, Spinner, Table } from "../../ui.tsx";

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

  return (
    <Card title="Connections">
      <div class="nc-stack">
        <div class="nc-row">
          <Field label="Type">
            <Select
              value={type()}
              options={[{ value: "", label: "Choose a type" }, ...(types()?.types ?? []).map((connectionType) => ({
                value: connectionType.id,
                label: connectionType.plugin === "" ? connectionType.label : `${connectionType.label} (${connectionType.plugin})`,
              }))]}
              onChange={setType}
            />
          </Field>
          <Field label="Name">
            <Input id="connection-name" value={name()} placeholder="lab-registry" onInput={setName} />
          </Field>
        </div>
        <Show when={chosenType()}>
          {connectionType => (
            <div class="nc-row">
              <For each={connectionType().fields}>
                {field => (
                  <Field label={field.label || field.name}>
                    <Input
                      id={`connection-field-${field.name}`}
                      type={field.secret ? "password" : "text"}
                      autocomplete={field.secret ? "new-password" : undefined}
                      value={values()[field.name] ?? ""}
                      onInput={(value) => setValue(field.name, value)}
                    />
                  </Field>
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

        <Show when={connections() !== undefined} fallback={<Spinner />}>
          <Show
            when={(connections()?.connections.length ?? 0) > 0}
            fallback={<Empty title="No connections" description="A connection is a login a host or a plugin uses, kept sealed." />}
          >
            <Table columns={["Name", "Type", ""]}>
              <For each={connections()?.connections ?? []}>
                {connection => (
                  <tr>
                    <td class="nc-mono">{connection.name}</td>
                    <td>
                      <Badge tone="outline">{connection.type}</Badge>
                    </td>
                    <td>
                      <div class="nc-row nc-row--tight">
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => {
                            setTested("");
                            void shell.rookery.connections
                              .test({ connection: connection.id })
                              .then((answer) => setTested(`${connection.name}: ${answer.ok ? "works" : "does not work"}${answer.message === "" ? "" : ` — ${answer.message}`}`))
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
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </div>
    </Card>
  );
}
