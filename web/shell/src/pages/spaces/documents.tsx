// Documents: the live documents of a space, or in Administration every one the
// account may read, with their metadata and versions. Editing one is a CRDT
// client's job; the shell shows what a document is and where it hangs. The
// open document is in the address.
import { For, Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Document } from "@rookery/proto";
import { Badge, Button, Card, CodeBlock, EmptyState, ListRow, PropertyList, RowList, Select, Skeleton, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../parts/dialogs.tsx";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { inSpace } from "../../spaces.ts";
import { dateOf, relativeTime } from "../fleet/format.ts";

/** DOCUMENT_TYPES are the CRDTs a document can be. */
const DOCUMENT_TYPES = [
  { value: "yjs", label: "Yjs", description: "Structured data and rich text." },
  { value: "markdown", label: "Markdown", description: "Plain text." },
];

/** DocumentsPage lists, makes and opens documents. */
export function DocumentsPage(): JSX.Element {
  const shell = useShell();
  // Inside a space the page holds that space's documents; elsewhere, every one.
  const space = untrack(() => shell.spaces.current());
  const parent = space === "" ? "" : `space://${space}`;
  const [search, setSearch] = useSearchParams<{ document?: string }>();
  const documents = createLive(() => shell.rookery.documents.list({ parent, limit: 200 }));
  const [creating, setCreating] = createSignal(false);
  const [deleting, setDeleting] = createSignal<Document>();
  const rows = () => documents()?.documents ?? [];
  const open = () => rows().find((one) => one.id === search.document);
  const refresh = () => void documents.refresh().catch(() => undefined);
  const remove = createAction(async () => {
    const document = deleting();
    if (document === undefined) return;
    await shell.rookery.documents.delete({ id: document.id });
    if (search.document === document.id) setSearch({ document: undefined });
    refresh();
  }, { done: "Document deleted" });

  const menu = [{ id: "delete", label: "Delete", icon: "trash" as const, danger: true }];

  return (
    <View
      id="documents"
      crumbs={inSpace(shell.spaces, [{ label: "Documents" }])}
      error={documents.error() === undefined ? undefined : `The documents could not be read. ${errorMessage(documents.error())}`}
      actions={[
        { id: "new", label: "New document", icon: "plus", onClick: () => setCreating(true) },
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: refresh },
      ]}
      aside={
        <Show when={open()} keyed>
          {(document) => <DocumentDetail document={document} />}
        </Show>
      }
      asideLabel="Document"
    >
      <Show when={documents()} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState icon="file" title="No documents">
                <Button icon="plus" onClick={() => setCreating(true)}>
                  New document
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Documents">
              <For each={rows()} keyed={(document) => document.id}>
                {(document) => (
                  <ListRow
                    icon="file"
                    title={<span class="nc-mono">{document().id}</span>}
                    label={document().id}
                    secondary={parent === "" ? <span class="nc-mono">{document().parent}</span> : undefined}
                    meta={
                      <span class="nc-row nc-row--tight">
                        <Badge tone="outline">{document().type}</Badge>
                        <span>{relativeTime(dateOf(document().updatedAt))}</span>
                      </span>
                    }
                    selected={search.document === document().id}
                    onClick={() => setSearch({ document: document().id })}
                    menu={menu}
                    onMenuSelect={() => setDeleting(document())}
                    persistentActions={<RowActions label={`Actions for ${document().id}`} items={menu} onSelect={() => setDeleting(document())} />}
                  />
                )}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>
      <Show when={creating()}>
        <NewDocument
          parent={parent}
          onClose={() => setCreating(false)}
          onCreated={(id) => {
            refresh();
            setSearch({ document: id });
          }}
        />
      </Show>
      <ConfirmDialog
        open={deleting() !== undefined}
        title="Delete the document?"
        description="Anyone editing it is disconnected."
        confirm="Delete document"
        danger
        action={remove}
        onClose={() => setDeleting(undefined)}
      />
    </View>
  );
}

function DocumentDetail(props: { document: Document }): JSX.Element {
  const shell = useShell();
  const id = untrack(() => props.document.id);
  const history = createLive(() => shell.rookery.documents.history({ id }));
  return (
    <div class="shell-view">
      <PropertyList
        items={[
          { label: "Type", value: <Badge tone="outline">{props.document.type}</Badge> },
          { label: "Parent", value: <span class="nc-mono">{props.document.parent}</span> },
          { label: "Created", value: relativeTime(dateOf(props.document.createdAt)) },
          { label: "Updated", value: relativeTime(dateOf(props.document.updatedAt)) },
        ]}
      />
      <CodeBlock title="Sync address" lineNumbers={false} code={`/ws/documents/${id}`} />
      <Show when={(history()?.snapshots.length ?? 0) > 0}>
      <Card title="Versions" count={history()?.snapshots.length} padding="none">
        <Show when={history()}>
          {(answer) => (
              <RowList label="Versions">
                <For each={answer().snapshots}>
                  {(snapshot) => (
                    <ListRow
                      title={snapshot.label || `Version ${snapshot.sequence}`}
                      secondary={<span class="nc-mono">{snapshot.createdBy}</span>}
                      meta={relativeTime(dateOf(snapshot.at))}
                    />
                  )}
                </For>
              </RowList>
          )}
        </Show>
      </Card>
      </Show>
    </div>
  );
}

function NewDocument(props: { parent: string; onClose: () => void; onCreated: (id: string) => void }): JSX.Element {
  const shell = useShell();
  const fixed = untrack(() => props.parent);
  const [type, setType] = createSignal("yjs");
  const [parent, setParent] = createSignal(fixed);
  let created = "";
  const create = createAction(async () => {
    const answer = await shell.rookery.documents.create({ type: type(), parent: parent().trim() });
    created = answer.document?.id ?? "";
  }, { done: "Document created" });
  return (
    <FormDialog
      open
      title="New document"
      submit="Create document"
      action={create}
      disabled={parent().trim() === ""}
      onClose={props.onClose}
      onDone={() => props.onCreated(created)}
    >
      <div class="nc-stack">
        <Select label="Type" value={type()} options={DOCUMENT_TYPES} onChange={setType} />
        <Show when={fixed === ""}>
          <TextField
            label="Parent"
            mono
            placeholder="space://…"
            info="Whoever may read the parent may read the document."
            value={parent()}
            onInput={(event) => setParent(event.currentTarget.value)}
          />
        </Show>
      </div>
    </FormDialog>
  );
}
