// Secrets: the deployment's sealed values, by name and scope. A value is never
// read back, so the page shows names only.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Empty, Field, Input, Select, Spinner, Table } from "../../ui.tsx";

/** SCOPES are the places a secret can belong to. */
const SCOPES = [
  { value: "global", label: "Deployment-wide" },
  { value: "plugin", label: "A plugin" },
  { value: "provisioner", label: "A provisioner" },
  { value: "host", label: "A host" },
];

/** SecretsSection lists the secrets and sets, renames and removes them. */
export function SecretsSection(): JSX.Element {
  const shell = useShell();
  const secrets = createQuery(() => shell.rookery.secrets.list({}));

  const [name, setName] = createSignal("");
  const [scope, setScope] = createSignal("global");
  const [subject, setSubject] = createSignal("");
  const [value, setValue] = createSignal("");
  const [error, setError] = createSignal("");
  const [message, setMessage] = createSignal("");

  const scopeOf = () => (scope() === "global" ? "global" : `${scope()}/${subject().trim()}`);

  const set = async () => {
    setError("");
    setMessage("");
    if (name().trim() === "") return;
    try {
      await shell.rookery.secrets.set({
        name: name().trim(),
        scope: scopeOf(),
        value: new TextEncoder().encode(value()),
      });
      setValue("");
      setMessage(`Set ${name().trim()}.`);
      void secrets.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  return (
    <Card title="Secrets">
      <div class="nc-stack">
        <div class="nc-row">
          <Field label="Name">
            <Input id="secret-name" value={name()} placeholder="github_token" onInput={setName} />
          </Field>
          <Field label="Scope">
            <Select value={scope()} options={SCOPES} onChange={setScope} />
          </Field>
          <Show when={scope() !== "global"}>
            <Field label={scope() === "host" ? "Host" : scope() === "plugin" ? "Plugin" : "Provisioner"}>
              <Input id="secret-subject" value={subject()} placeholder="studio" onInput={setSubject} />
            </Field>
          </Show>
          <Field label="Value">
            <Input id="secret-value" type="password" value={value()} autocomplete="new-password" onInput={setValue} />
          </Field>
          <Button variant="primary" size="sm" onClick={() => void set()}>
            Set
          </Button>
        </div>
        <Show when={message() !== ""}>
          <Callout tone="info">{message()}</Callout>
        </Show>
        <Show when={error() !== ""}>
          <Callout tone="danger">{error()}</Callout>
        </Show>

        <Show when={secrets() !== undefined} fallback={<Spinner />}>
          <Show
            when={(secrets()?.secrets.length ?? 0) > 0}
            fallback={<Empty title="No secrets" description="A secret reaches only what its scope names: a plugin, a provisioner, a host or the deployment." />}
          >
            <Table columns={["Name", "Scope", ""]}>
              <For each={secrets()?.secrets ?? []}>
                {secret => (
                  <tr>
                    <td class="nc-mono">{secret.name}</td>
                    <td>
                      <Badge tone="outline">{secret.scope}</Badge>
                    </td>
                    <td>
                      <div class="nc-row nc-row--tight">
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => {
                            const next = window.prompt("New name", secret.name);
                            if (next === null || next.trim() === "") return;
                            void shell.rookery.secrets
                              .rename({ name: secret.name, scope: secret.scope, newName: next.trim() })
                              .then(() => secrets.refresh());
                          }}
                        >
                          Rename
                        </Button>
                        <Button
                          size="sm"
                          variant="danger"
                          onClick={() => {
                            void shell.rookery.secrets.delete({ name: secret.name, scope: secret.scope }).then(() => secrets.refresh());
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
