// Every space, archived ones too, for admins: rename, archive and delete them.
import { For, Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Space } from "@rookery/proto";
import { Badge, Button, Card, EmptyState, ListRow, RowList, Skeleton, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { spacePath } from "../../spaces.ts";
import { ADMIN } from "../admin/people.tsx";
import { openNewSpace } from "./sidebar.tsx";

/** SpacesPage lists the spaces, and makes, renames, archives and deletes them. */
export function SpacesPage(): JSX.Element {
  const shell = useShell();
  const spaces = createLive(() => shell.rookery.spaces.list({ includeArchived: true, limit: 500 }));
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
    if (id === "open") shell.navigate(spacePath(space.id));
    if (id === "rename") setRenaming(space);
    if (id === "archive") void archive.run(space);
    if (id === "delete") setDeleting(space);
  };

  return (
    <View
      id="spaces"
      crumbs={[ADMIN, { label: "Spaces" }]}
      error={spaces.error() === undefined ? undefined : `The spaces could not be read. ${errorMessage(spaces.error())}`}
      actions={[
        ...(shell.spaces.kinds().length > 0 ? [{ id: "new", label: "New space", icon: "plus" as const, onClick: openNewSpace }] : []),
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: refresh },
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
              >
                <Show when={shell.spaces.kinds().length > 0}>
                  <Button icon="plus" onClick={openNewSpace}>
                    New space
                  </Button>
                </Show>
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
                        <Show when={space().archived}>
                          <Badge>archived</Badge>
                        </Show>
                      </span>
                    }
                    href={space().archived ? undefined : spacePath(space().id)}
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

      <Show when={renaming()} keyed>
        {(space) => <RenameDialog space={space} onClose={() => setRenaming(undefined)} onRenamed={refresh} />}
      </Show>
      <ConfirmDialog
        open={deleting() !== undefined}
        title={`Delete ${deleting()?.name ?? "the space"}?`}
        description="Its records and documents go with it."
        confirm="Delete space"
        danger
        action={remove}
        onClose={() => setDeleting(undefined)}
      />
    </View>
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
