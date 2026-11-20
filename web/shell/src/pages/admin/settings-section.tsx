// One setting at a time: what it is, what it is now, and where its value comes
// from.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { createQuery } from "@rookery/client/solid";
import type { Setting } from "@rookery/proto";
import { Badge, Button, Callout, Card, DataTable, type DataColumn, Select, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";

/** SettingsSection is the settings the core and the plugins declare. */
export function SettingsSection(): JSX.Element {
  const shell = useShell();
  const settings = createQuery(() => shell.rookery.settings.list({}));

  const [drafts, setDrafts] = createSignal<Record<string, string>>({});
  const [scope, setScope] = createSignal("global");
  const [error, setError] = createSignal("");

  const draft = (setting: Setting): string => drafts()[setting.key] ?? textOf(setting.valueCbor);

  const setDraft = (key: string, value: string) => setDrafts((current) => ({ ...current, [key]: value }));

  const save = async (setting: Setting) => {
    setError("");
    try {
      await shell.rookery.settings.set({
        key: setting.key,
        scope: scope(),
        space: scope() === "space" ? shell.spaces.current() : "",
        valueCbor: cbor.encode(valueFor(setting, draft(setting))),
      });
      void settings.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const columns = (): DataColumn<Setting>[] => [
    { id: "key", header: "Setting", type: "mono", accessor: setting => setting.key },
    {
      id: "type",
      header: "Type",
      accessor: setting => setting.type,
      cell: setting => <Badge tone="outline">{setting.type}</Badge>,
    },
    {
      id: "value",
      header: "Value",
      cell: setting => (
        <Show
          when={setting.type === "bool"}
          fallback={
            <TextField
              id={`setting-${setting.key.replaceAll("/", "-")}`}
              aria-label={setting.key}
              value={draft(setting)}
              onInput={event => setDraft(setting.key, event.currentTarget.value)}
            />
          }
        >
          <Select
            aria-label={setting.key}
            value={draft(setting) === "true" ? "true" : "false"}
            options={[
              { value: "true", label: "on" },
              { value: "false", label: "off" },
            ]}
            onChange={value => setDraft(setting.key, value)}
          />
        </Show>
      ),
    },
    {
      id: "from",
      header: "From",
      accessor: setting => setting.scope,
      cell: setting => <Badge tone={setting.scope === "default" ? "outline" : "info"}>{setting.scope}</Badge>,
    },
    {
      id: "actions",
      header: "",
      cell: setting => (
        <div class="nc-row nc-row--tight">
          <Button size="sm" onClick={() => void save(setting)}>
            Save
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void shell.rookery.settings
                .reset({ key: setting.key, scope: scope(), space: scope() === "space" ? shell.spaces.current() : "" })
                .then(() => settings.refresh());
            }}
          >
            Reset
          </Button>
        </div>
      ),
    },
  ];

  return (
    <>
      <Card
        title="Settings"
        padding="none"
        action={
          <Select
            aria-label="The scope a value is saved in"
            value={scope()}
            options={[
              { value: "global", label: "Deployment-wide" },
              { value: "space", label: "This space" },
              { value: "user", label: "Only me" },
            ]}
            onChange={setScope}
          />
        }
      >
        <DataTable
          bare
          label="Settings"
          rows={settings()?.settings ?? []}
          columns={columns()}
          getRowId={setting => setting.key}
          loading={settings.pending() && settings() === undefined}
          empty={{
            title: "No settings",
            description: "The core and each installed plugin declare the settings they take.",
          }}
          error={
            settings.error() === undefined
              ? undefined
              : { message: "The settings could not be read.", detail: settings.error()?.message }
          }
        />
      </Card>
      <Show when={error() !== ""}>
        <Callout tone="danger">{error()}</Callout>
      </Show>
    </>
  );
}

/** textOf is a CBOR value as the text in an input. */
function textOf(bytes: Uint8Array): string {
  const value = cbor.decode<unknown>(bytes);
  if (value === undefined || value === null) return "";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "string" || typeof value === "number") return String(value);
  return JSON.stringify(value);
}

/** valueFor is what a setting's type makes of the text an admin typed. */
function valueFor(setting: Setting, text: string): unknown {
  switch (setting.type) {
    case "int":
      return Number.parseInt(text, 10);
    case "float":
      return Number.parseFloat(text);
    case "bool":
      return text === "true";
    default:
      return text;
  }
}
