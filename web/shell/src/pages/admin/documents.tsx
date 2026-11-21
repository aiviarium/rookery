// Documents: the live documents this account may read, their metadata and their
// versions. Editing one is a CRDT client's job; the shell shows what a document
// is and where it hangs. The open document is in the address.
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
import { dateOf, relativeTime } from "../infra/format.ts";

/** DOCUMENT_TYPES are the CRDTs a document can be. */
const DOCUMENT_TYPES = [
  { value: "yjs", label: "Yjs", description: "Structured data and rich text." },
  { value: "markdown", label: "Markdown", description: "Plain text." },
];

/** DocumentsPage lists, makes and opens documents. */
export function DocumentsPage(): JSX.Element {
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ document?: string }>();
  const documents = createLive(() => shell.rookery.documents.list({ limit: 200 }));
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
      crumbs={[{ label: "Documents" }]}
      error={documents.error() === undefined ? undefined : `The documents could not be read. ${errorMessage(documents.error())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: refresh },
        { id: "new", label: "New document", icon: "plus", primary: true, onClick: () => setCreating(true) },
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
              <EmptyState icon="file" title="No documents" description="A document is edited live by everyone who opens it, and its grants come from its parent.">
                <Button variant="primary" icon="plus" onClick={() => setCreating(true)}>
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
                    secondary={<span class="nc-mono">{document().parent}</span>}
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
        description="Its content and versions are deleted. Anyone editing it is disconnected."
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
      <Card title="Versions" count={history()?.snapshots.length}>
        <Show when={history()} fallback={<Skeleton width="60%" />}>
          {(answer) => (
            <Show when={answer().snapshots.length > 0} fallback={<p class="nc-muted">No versions yet. An editor marks one when it compacts the document.</p>}>
              <RowList label="Versions" variant="inline">
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
            </Show>
          )}
        </Show>
      </Card>
    </div>
  );
}

function NewDocument(props: { onClose: () => void; onCreated: (id: string) => void }): JSX.Element {
  const shell = useShell();
  const space = shell.spaces.current();
  const [type, setType] = createSignal("yjs");
  const [parent, setParent] = createSignal(space === "" ? "" : `space://${space}`);
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
        <TextField
          label="Parent"
          mono
          placeholder="space://…"
          help="Whoever may read the parent may read the document."
          value={parent()}
          onInput={(event) => setParent(event.currentTarget.value)}
        />
      </div>
    </FormDialog>
  );
}
