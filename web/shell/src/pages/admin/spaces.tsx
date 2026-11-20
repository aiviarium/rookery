// Spaces: the containers records and documents are scoped to.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Space } from "@rookery/proto";
import { Badge, Button, Callout, Card, DataTable, type DataColumn, Select, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

/** SpacesPage lists the spaces, and makes, renames, archives and deletes them. */
export function SpacesPage(): JSX.Element {
  const shell = useShell();
  const spaces = createQuery(() => shell.rookery.spaces.list({ includeArchived: true, limit: 500 }));
  const kinds = createQuery(() => shell.rookery.spaces.listKinds({}));

  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal("");
  const [error, setError] = createSignal("");
  const [renaming, setRenaming] = createSignal("");
  const [newName, setNewName] = createSignal("");

  const refreshAll = () => {
    void spaces.refresh().catch(() => undefined);
    void shell.spaces.refresh();
  };

  const create = async () => {
    setError("");
    if (name().trim() === "" || kind() === "") return;
    try {
      const answer = await shell.rookery.spaces.create({ kind: kind(), name: name().trim() });
      setName("");
      refreshAll();
      // A space is a place to be in: open the new one.
      if (answer.space !== undefined) shell.spaces.select(answer.space.id);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const columns = (): DataColumn<Space>[] => [
    {
      id: "name",
      header: "Name",
      accessor: space => space.name,
      cell: space => (
        <Show when={renaming() === space.id} fallback={<strong>{space.name}</strong>}>
          <div class="nc-row nc-row--tight">
            <TextField
              aria-label={`The new name of ${space.name}`}
              id={`rename-${space.id}`}
              value={newName()}
              onInput={event => setNewName(event.currentTarget.value)}
            />
            <Button
              size="sm"
              onClick={() => {
                void shell.rookery.spaces.rename({ space: space.id, name: newName() }).then(() => {
                  setRenaming("");
                  refreshAll();
                });
              }}
            >
              Save
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setRenaming("")}>
              Cancel
            </Button>
          </div>
        </Show>
      ),
    },
    { id: "kind", header: "Kind", accessor: space => space.kind },
    {
      id: "state",
      header: "State",
      accessor: space => (space.archived ? "archived" : "open"),
      cell: space => (
        <Show when={space.archived} fallback={<Badge tone="success">open</Badge>}>
          <Badge>archived</Badge>
        </Show>
      ),
    },
    {
      id: "actions",
      header: "",
      cell: space => (
        <div class="nc-row nc-row--tight">
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setRenaming(space.id);
              setNewName(space.name);
            }}
          >
            Rename
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void shell.rookery.spaces.archive({ space: space.id, archived: !space.archived }).then(refreshAll);
            }}
          >
            {space.archived ? "Unarchive" : "Archive"}
          </Button>
          <Button
            size="sm"
            variant="danger"
            onClick={() => {
              void shell.rookery.spaces.delete({ space: space.id }).then(refreshAll);
            }}
          >
            Delete
          </Button>
        </div>
      ),
    },
  ];

  return (
    <Page title="Spaces" description="A space gathers records and documents, and gives them one place to live.">
      <Card title="New space">
        <div class="nc-stack">
          <div class="nc-row">
            <Select
              label="Kind"
              value={kind()}
              options={[
                { value: "", label: "Choose a kind" },
                ...(kinds()?.kinds ?? []).map((spaceKind) => ({
                  value: spaceKind.id,
                  label: spaceKind.plugin === "" ? spaceKind.label : `${spaceKind.label} (${spaceKind.plugin})`,
                })),
              ]}
              onChange={setKind}
            />
            <TextField label="Name" id="space-name" value={name()} placeholder="Analysis" onInput={event => setName(event.currentTarget.value)} />
            <Button variant="primary" size="sm" onClick={() => void create()}>
              Create
            </Button>
          </div>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
          <Show when={kinds()?.kinds.length === 0}>
            <p class="nc-meta">No kind of space is declared yet. A plugin adds one with its manifest.</p>
          </Show>
        </div>
      </Card>

      <Card title="Spaces" count={spaces()?.spaces.length} padding="none">
        <DataTable
          bare
          label="Spaces"
          rows={spaces()?.spaces ?? []}
          columns={columns()}
          getRowId={space => space.id}
          loading={spaces.pending() && spaces() === undefined}
          empty={{ title: "No spaces", description: "Make one above, or ask a plugin's page to create one." }}
        />
      </Card>
    </Page>
  );
}
