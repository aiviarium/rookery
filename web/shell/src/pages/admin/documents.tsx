// Documents: the live documents this account may read, their metadata and their
// versions. Editing one is a CRDT client's job; the shell shows what a document
// is and where it hangs.
import { Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import type { Timestamp } from "@bufbuild/protobuf/wkt";
import type { Document, DocumentSnapshot } from "@rookery/proto";
import { Badge, Button, Callout, Card, DataTable, type DataColumn, Select, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

/** DocumentTypes are the CRDTs a document can be. */
const DOCUMENT_TYPES = [
  { value: "yjs", label: "Yjs" },
  { value: "markdown", label: "Markdown" },
];

/** DocumentsPage lists, makes and opens documents. */
export function DocumentsPage(): JSX.Element {
  const shell = useShell();
  const documents = createQuery(() => shell.rookery.documents.list({ limit: 200 }));

  const [type, setType] = createSignal("yjs");
  const [parent, setParent] = createSignal(shell.spaces.current() === "" ? "" : `space://${shell.spaces.current()}`);
  const [error, setError] = createSignal("");
  const [open, setOpen] = createSignal("");

  const history = createQuery(() =>
    open() === "" ? Promise.resolve(undefined) : shell.rookery.documents.history({ id: open() }),
  );

  // The versions are the open document's.
  createEffect(
    () => open(),
    (id) => {
      if (id !== "") void history.refresh().catch(() => undefined);
    },
  );

  const create = async () => {
    setError("");
    try {
      const answer = await shell.rookery.documents.create({ type: type(), parent: parent().trim() });
      if (answer.document !== undefined) setOpen(answer.document.id);
      void documents.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const columns = (): DataColumn<Document>[] => [
    { id: "id", header: "Id", type: "mono", accessor: document => document.id },
    {
      id: "type",
      header: "Type",
      accessor: document => document.type,
      cell: document => <Badge tone="outline">{document.type}</Badge>,
    },
    { id: "parent", header: "Parent", type: "mono", accessor: document => document.parent },
    { id: "updated", header: "Updated", type: "time", accessor: document => when(document.updatedAt) },
    {
      id: "actions",
      header: "",
      cell: document => (
        <div class="nc-row nc-row--tight">
          <Button size="sm" variant="ghost" onClick={() => setOpen(document.id)}>
            Open
          </Button>
          <Button
            size="sm"
            variant="danger"
            onClick={() => {
              void shell.rookery.documents.delete({ id: document.id }).then(() => documents.refresh());
            }}
          >
            Delete
          </Button>
        </div>
      ),
    },
  ];

  const snapshotColumns = (): DataColumn<DocumentSnapshot>[] => [
    { id: "sequence", header: "Sequence", type: "num", accessor: snapshot => String(snapshot.sequence) },
    { id: "label", header: "Label", accessor: snapshot => snapshot.label },
    { id: "by", header: "By", type: "mono", accessor: snapshot => snapshot.createdBy },
    { id: "at", header: "At", type: "time", accessor: snapshot => when(snapshot.at) },
  ];

  return (
    <Page title="Documents" description="Live documents, kept per parent resource and every participant's edits.">
      <Card title="New document">
        <div class="nc-stack">
          <div class="nc-row">
            <Select label="Type" value={type()} options={DOCUMENT_TYPES} onChange={setType} />
            <TextField
              label="Parent"
              help="The resource whose grants reach the document, such as space://…"
              id="document-parent"
              value={parent()}
              placeholder="space://…"
              onInput={event => setParent(event.currentTarget.value)}
            />
            <Button variant="primary" size="sm" onClick={() => void create()}>
              Create
            </Button>
          </div>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
        </div>
      </Card>

      <Card title="Documents" count={documents()?.documents.length} padding="none">
        <DataTable
          bare
          label="Documents"
          rows={documents()?.documents ?? []}
          columns={columns()}
          getRowId={document => document.id}
          loading={documents.pending() && documents() === undefined}
          empty={{ title: "No documents", description: "Make one above; the editor is the client that syncs it." }}
        />
      </Card>

      <Show when={open() !== ""}>
        <Card title="Versions" padding="none">
          <div class="shell-pad nc-stack">
            <p class="nc-meta">
              A client syncs a document at <code class="nc-code">/ws/documents/{open()}</code>.
            </p>
          </div>
          <Show
            when={(history()?.snapshots.length ?? 0) > 0}
            fallback={
              <div class="shell-pad">
                <Callout tone="info">No versions yet. A participant marks one when it compacts the document.</Callout>
              </div>
            }
          >
            <DataTable
              bare
              label="Versions"
              rows={history()?.snapshots ?? []}
              columns={snapshotColumns()}
              getRowId={snapshot => String(snapshot.sequence)}
              empty={{ title: "No versions" }}
            />
          </Show>
        </Card>
      </Show>
    </Page>
  );
}

/** when is a timestamp as the reader's clock shows it. */
function when(at: Timestamp | undefined): string {
  return at === undefined ? "" : timestampDate(at).toLocaleString();
}
