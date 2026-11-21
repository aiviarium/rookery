// Spaces: the containers records and documents are scoped to. A space is made
// from a kind a plugin declares; making one opens it.
import { For, Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Space } from "@rookery/proto";
import { Badge, Button, Card, EmptyState, ListRow, RowList, Select, Skeleton, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";

/** SpacesPage lists the spaces, and makes, renames, archives and deletes them. */
export function SpacesPage(): JSX.Element {
  const shell = useShell();
  const spaces = createLive(() => shell.rookery.spaces.list({ includeArchived: true, limit: 500 }));
  const [creating, setCreating] = createSignal(false);
  const [renaming, setRenaming] = createSignal<Space>();
  const [deleting, setDeleting] = createSignal<Space>();
  const rows = () => spaces()?.spaces ?? [];
  const refresh = () => {
    void spaces.refresh().catch(() => undefined);
    void shell.spaces.refresh();
  };

  const archive = createAction(async (space: Space) => {
    await shell.rookery.spaces.archive({ space: space.id, archived: !space.archived });
    refresh();
  }, { failed: "The space did not change" });
  const remove = createAction(async () => {
    const space = deleting();
    if (space !== undefined) await shell.rookery.spaces.delete({ space: space.id });
    refresh();
  }, { done: "Space deleted" });

  const menu = (space: Space) => [
    { id: "open", label: "Open", disabled: space.archived },
    { id: "rename", label: "Rename", icon: "edit" as const },
    { id: "archive", label: space.archived ? "Unarchive" : "Archive", icon: "archive" as const },
    { type: "separator" as const },
    { id: "delete", label: "Delete", icon: "trash" as const, danger: true },
  ];
  const choose = (space: Space, id: string) => {
    if (id === "open") shell.spaces.select(space.id);
    if (id === "rename") setRenaming(space);
    if (id === "archive") void archive.run(space);
    if (id === "delete") setDeleting(space);
  };

  return (
    <View
      id="spaces"
      crumbs={[{ label: "Spaces" }]}
      error={spaces.error() === undefined ? undefined : `The spaces could not be read. ${errorMessage(spaces.error())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: refresh },
        { id: "new", label: "New space", icon: "plus", primary: true, onClick: () => setCreating(true) },
      ]}
    >
      <Show when={spaces() !== undefined} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState
                icon="layers"
                title="No spaces"
                description="A space gathers records and documents in one place. Plugins declare the kinds there are."
              >
                <Button variant="primary" icon="plus" onClick={() => setCreating(true)}>
                  New space
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Spaces">
              <For each={rows()} keyed={(space) => space.id}>
                {(space) => (
                  <ListRow
                    data-space={space().name}
                    icon="layers"
                    title={space().name}
                    secondary={space().kind}
                    muted={space().archived}
                    status={
                      <span class="nc-row nc-row--tight">
                        <Show when={shell.spaces.current() === space().id}>
                          <Badge tone="accent">current</Badge>
                        </Show>
                        <Show when={space().archived}>
                          <Badge>archived</Badge>
                        </Show>
                      </span>
                    }
                    onClick={space().archived ? undefined : () => shell.spaces.select(space().id)}
                    menu={menu(space())}
                    menuLabel={`Actions for ${space().name}`}
                    onMenuSelect={(id) => choose(space(), id)}
                    persistentActions={
                      <RowActions label={`Actions for ${space().name}`} items={menu(space())} onSelect={(id) => choose(space(), id)} />
                    }
                  />
                )}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>

      <Show when={creating()}>
        <NewSpaceDialog onClose={() => setCreating(false)} onCreated={refresh} />
      </Show>
      <Show when={renaming()} keyed>
        {(space) => <RenameDialog space={space} onClose={() => setRenaming(undefined)} onRenamed={refresh} />}
      </Show>
      <ConfirmDialog
        open={deleting() !== undefined}
        title={`Delete ${deleting()?.name ?? "the space"}?`}
        description="Its records and documents are deleted with it. Archive it instead to keep them."
        confirm="Delete space"
        danger
        action={remove}
        onClose={() => setDeleting(undefined)}
      />
    </View>
  );
}

function NewSpaceDialog(props: { onClose: () => void; onCreated: () => void }): JSX.Element {
  const shell = useShell();
  const kinds = createLive(() => shell.rookery.spaces.listKinds({}));
  const [kind, setKind] = createSignal("");
  const [name, setName] = createSignal("");
  const options = () =>
    (kinds()?.kinds ?? []).map((one) => ({
      value: one.id,
      label: one.label,
      description: one.plugin === "" ? undefined : `From ${one.plugin}`,
    }));
  const create = createAction(async () => {
    const answer = await shell.rookery.spaces.create({ kind: kind(), name: name().trim() });
    props.onCreated();
    // A space is a place to be in: open the new one.
    if (answer.space !== undefined) shell.spaces.select(answer.space.id);
  }, { done: "Space created" });
  return (
    <FormDialog
      open
      title="New space"
      submit="Create space"
      action={create}
      disabled={kind() === "" || name().trim() === ""}
      onClose={props.onClose}
    >
      <div class="nc-stack">
        <Select
          label="Kind"
          placeholder={kinds() === undefined ? "Loading…" : "Choose a kind"}
          value={kind()}
          options={options()}
          help={kinds()?.kinds.length === 0 ? "No plugin declares a kind of space yet." : undefined}
          onChange={setKind}
        />
        <TextField label="Name" placeholder="Analysis" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
      </div>
    </FormDialog>
  );
}

function RenameDialog(props: { space: Space; onClose: () => void; onRenamed: () => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal(untrack(() => props.space.name));
  const rename = createAction(async () => {
    await shell.rookery.spaces.rename({ space: props.space.id, name: name().trim() });
  });
  return (
    <FormDialog
      open
      title={`Rename ${props.space.name}`}
      submit="Rename"
      action={rename}
      disabled={name().trim() === "" || name().trim() === props.space.name}
      onClose={props.onClose}
      onDone={props.onRenamed}
    >
      <TextField label="Name" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
    </FormDialog>
  );
}
