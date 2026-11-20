// Secrets: the deployment's sealed values, by name and scope. A value is never
// read back, so the page shows names only.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Secret } from "@rookery/proto";
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

  const columns = (): DataColumn<Secret>[] => [
    { id: "name", header: "Name", type: "mono", accessor: secret => secret.name },
    {
      id: "scope",
      header: "Scope",
      accessor: secret => secret.scope,
      cell: secret => <Badge tone="outline">{secret.scope}</Badge>,
    },
    {
      id: "actions",
      header: "",
      cell: secret => (
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
      ),
    },
  ];

  return (
    <Card title="Secrets" padding="none">
      <div class="shell-pad nc-stack">
        <div class="nc-row">
          <TextField label="Name" id="secret-name" value={name()} placeholder="github_token" onInput={event => setName(event.currentTarget.value)} />
          <Select label="Scope" value={scope()} options={SCOPES} onChange={setScope} />
          <Show when={scope() !== "global"}>
            <TextField
              label={scope() === "host" ? "Host" : scope() === "plugin" ? "Plugin" : "Provisioner"}
              id="secret-subject"
              value={subject()}
              placeholder="studio"
              onInput={event => setSubject(event.currentTarget.value)}
            />
          </Show>
          <TextField
            label="Value"
            id="secret-value"
            type="password"
            autocomplete="new-password"
            value={value()}
            onInput={event => setValue(event.currentTarget.value)}
          />
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
      </div>
      <DataTable
        bare
        label="Secrets"
        rows={secrets()?.secrets ?? []}
        columns={columns()}
        getRowId={secret => `${secret.scope}/${secret.name}`}
        loading={secrets.pending() && secrets() === undefined}
        empty={{
          title: "No secrets",
          description: "A secret reaches only what its scope names: a plugin, a provisioner, a host or the deployment.",
        }}
      />
    </Card>
  );
}
