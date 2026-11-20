// Documents: the live documents this account may read, their metadata and their
// versions. Editing one is a CRDT client's job; the shell shows what a document
// is and where it hangs.
import { For, Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { timestampDate } from "@bufbuild/protobuf/wkt";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Code, Empty, Field, Input, Page, Select, Spinner, Table } from "../../ui.tsx";
import type { Timestamp } from "@bufbuild/protobuf/wkt";

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

  return (
    <Page title="Documents" description="Live documents, kept per parent resource and every participant's edits.">
      <Card title="New document">
        <div class="nc-stack">
          <div class="nc-row">
            <Field label="Type">
              <Select value={type()} options={DOCUMENT_TYPES} onChange={setType} />
            </Field>
            <Field label="Parent" hint="The resource whose grants reach the document, such as space://…">
              <Input id="document-parent" value={parent()} placeholder="space://…" onInput={setParent} />
            </Field>
            <Button variant="primary" size="sm" onClick={() => void create()}>
              Create
            </Button>
          </div>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
        </div>
      </Card>

      <Card title="Documents">
        <Show when={documents() !== undefined} fallback={<Spinner />}>
          <Show
            when={(documents()?.documents.length ?? 0) > 0}
            fallback={<Empty title="No documents" description="Make one above; the editor is the client that syncs it." />}
          >
            <Table columns={["Id", "Type", "Parent", "Updated", ""]}>
              <For each={documents()?.documents ?? []}>
                {document => (
                  <tr aria-selected={open() === document.id ? "true" : "false"}>
                    <td class="nc-mono">{document.id}</td>
                    <td>
                      <Badge tone="outline">{document.type}</Badge>
                    </td>
                    <td class="nc-mono">{document.parent}</td>
                    <td>{when(document.updatedAt)}</td>
                    <td>
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
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>

      <Show when={open() !== ""}>
        <Card title="Versions">
          <p class="nc-meta">
            A client syncs a document at <code class="nc-code">/ws/documents/{open()}</code>.
          </p>
          <Show when={history() !== undefined} fallback={<Spinner />}>
            <Show
              when={(history()?.snapshots.length ?? 0) > 0}
              fallback={<Callout tone="info">No versions yet. A participant marks one when it compacts the document.</Callout>}
            >
              <Table columns={["Sequence", "Label", "By", "At"]}>
                <For each={history()?.snapshots ?? []}>
                  {snapshot => (
                    <tr>
                      <td>{String(snapshot.sequence)}</td>
                      <td>{snapshot.label}</td>
                      <td class="nc-mono">{snapshot.createdBy}</td>
                      <td>{when(snapshot.at)}</td>
                    </tr>
                  )}
                </For>
              </Table>
            </Show>
          </Show>
          <Code>{open()}</Code>
        </Card>
      </Show>
    </Page>
  );
}

/** when is a timestamp as the reader's clock shows it. */
function when(at: Timestamp | undefined): string {
  return at === undefined ? "" : timestampDate(at).toLocaleString();
}
