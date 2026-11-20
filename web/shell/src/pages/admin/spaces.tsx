// Spaces: the containers records and documents are scoped to.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Empty, Field, Input, Page, Select, Spinner, Table } from "../../ui.tsx";

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

  return (
    <Page title="Spaces" description="A space gathers records and documents, and gives them one place to live.">
      <Card title="New space">
        <div class="nc-stack">
          <div class="nc-row">
            <Field label="Kind">
              <Select
                value={kind()}
                options={[{ value: "", label: "Choose a kind" }, ...(kinds()?.kinds ?? []).map((spaceKind) => ({
                  value: spaceKind.id,
                  label: spaceKind.plugin === "" ? spaceKind.label : `${spaceKind.label} (${spaceKind.plugin})`,
                }))]}
                onChange={setKind}
              />
            </Field>
            <Field label="Name">
              <Input id="space-name" value={name()} placeholder="Analysis" onInput={setName} />
            </Field>
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

      <Card title="Spaces">
        <Show when={spaces() !== undefined} fallback={<Spinner />}>
          <Show
            when={(spaces()?.spaces.length ?? 0) > 0}
            fallback={<Empty title="No spaces" description="Make one above, or ask a plugin's page to create one." />}
          >
            <Table columns={["Name", "Kind", "State", ""]}>
              <For each={spaces()?.spaces ?? []}>
                {space => (
                  <tr>
                    <td>
                      <Show
                        when={renaming() === space.id}
                        fallback={<strong>{space.name}</strong>}
                      >
                        <div class="nc-row nc-row--tight">
                          <Input id={`rename-${space.id}`} value={newName()} onInput={setNewName} />
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
                    </td>
                    <td>{space.kind}</td>
                    <td>
                      <Show when={space.archived} fallback={<Badge tone="success">open</Badge>}>
                        <Badge>archived</Badge>
                      </Show>
                    </td>
                    <td>
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
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>
    </Page>
  );
}
