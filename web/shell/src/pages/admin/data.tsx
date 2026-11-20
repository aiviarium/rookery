// The datastore: the collections plugins declare, their records, a record's
// history, and the changes that arrive while the page is open.
import { For, Show, createEffect, createMemo, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { createQuery, useLiveQuery } from "@rookery/client/solid";
import type { Collection } from "@rookery/proto";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Code, Empty, Page, Select, Spinner, Table } from "../../ui.tsx";

/** DataPage browses the collections the caller may read. */
export function DataPage(): JSX.Element {
  const shell = useShell();
  const collections = createQuery(() =>
    shell.rookery.data.listCollections({ space: shell.spaces.current() }),
  );
  const [selected, setSelected] = createSignal("");

  const collection = (): Collection | undefined =>
    (collections()?.collections ?? []).find((candidate) => keyOf(candidate) === selected());

  return (
    <Page title="Datastore" description="The records of every collection this account may read.">
      <Card
        title="Collections"
        actions={
          <Select
            value={selected()}
            options={[
              { value: "", label: "Choose a collection" },
              ...(collections()?.collections ?? []).map((candidate) => ({
                value: keyOf(candidate),
                label: `${keyOf(candidate)} (${candidate.scope})`,
              })),
            ]}
            onChange={setSelected}
          />
        }
      >
        <Show when={collections() !== undefined} fallback={<Spinner />}>
          <Show
            when={(collections()?.collections.length ?? 0) > 0}
            fallback={
              <Empty
                title="No collections to read"
                description="A collection is declared by a plugin's manifest, and its records are checked against the space or plugin they belong to."
              />
            }
          >
            <Show when={collection()} fallback={<p class="nc-meta">Choose a collection to see its records.</p>}>
              {candidate => (
                <div class="nc-row" style={{ gap: "var(--space-3)" }}>
                  <Badge tone="outline">{candidate().scope}</Badge>
                  <Show when={candidate().history}>
                    <Badge tone="info">keeps history</Badge>
                  </Show>
                  <span class="nc-meta">{candidate().fields.join(", ")}</span>
                </div>
              )}
            </Show>
          </Show>
        </Show>
      </Card>

      <Show when={collection()} keyed>
        {candidate => <Records collection={candidate} />}
      </Show>
    </Page>
  );
}

/** Records is one collection's records, kept up to date by its live query. */
function Records(props: { collection: Collection }): JSX.Element {
  const shell = useShell();
  const key = keyOf(props.collection);
  const live = useLiveQuery<Record<string, unknown>>(shell.rookery.data, {
    collection: key,
    space: shell.spaces.current(),
    limit: 200,
  });
  const [open, setOpen] = createSignal("");
  const history = createQuery(() =>
    open() === "" ? Promise.resolve(undefined) : shell.rookery.data.history({ collection: key, id: open(), limit: 50 }),
  );

  // The history is the open record's, so it is asked again when another row is
  // chosen.
  createEffect(
    () => open(),
    (id) => {
      if (id !== "") void history.refresh().catch(() => undefined);
    },
  );

  const columns = createMemo(() => props.collection.fields.slice(0, 6));

  return (
    <div class="nc-split" style={{ gap: "var(--space-2)", "align-items": "flex-start" }}>
      <Card title={`${key}`} class="nc-split__pane nc-split__pane--fill">
        <Show when={live.status() !== "loading"} fallback={<Spinner />}>
          <Show
            when={live.records().length > 0}
            fallback={
              <Empty
                title="No records"
                description="Nothing in this collection matches, or the rows are in a space you are not in."
                actions={
                  <Button
                    size="sm"
                    onClick={() => {
                      live.restart();
                    }}
                  >
                    Ask again
                  </Button>
                }
              />
            }
          >
            <Table columns={["id", ...columns()]}>
              <For each={live.records()}>
                {row => (
                  <tr
                    aria-selected={open() === recordID(row.record) ? "true" : "false"}
                    style={{ cursor: "pointer" }}
                    onClick={() => setOpen(recordID(row.record))}
                  >
                    <td class="nc-mono">{recordID(row.record)}</td>
                    <For each={columns()}>
                      {field => <td>{formatValue(row.record[field])}</td>}
                    </For>
                  </tr>
                )}
              </For>
            </Table>
            <p class="nc-meta">
              {live.records().length} records, live: {live.status()}
            </p>
          </Show>
        </Show>
      </Card>

      <Card title="History" class="nc-split__pane nc-split__pane--sized" >
        <Show when={open() !== ""} fallback={<p class="nc-meta">Choose a record to see its revisions.</p>}>
          <Show when={history() !== undefined} fallback={<Spinner />}>
            <Show
              when={(history()?.entriesCbor.length ?? 0) > 0}
              fallback={<Callout tone="info">This collection keeps no history, or the record has one revision.</Callout>}
            >
              <div class="nc-stack">
                <For each={history()?.entriesCbor ?? []}>
                  {entry => (
                    <div class="nc-stack">
                      <div class="nc-row nc-row--tight">
                        <Badge tone="outline">rev {revisionOf(entry)}</Badge>
                        <span class="nc-meta">{principalOf(entry)}</span>
                      </div>
                      <Code>{JSON.stringify(cbor.decode<Record<string, unknown>>(entry)?.["record"] ?? {}, null, 2)}</Code>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </Show>
      </Card>
    </div>
  );
}

/** keyOf is a collection's "plugin/name". */
function keyOf(collection: Collection): string {
  return `${collection.plugin}/${collection.name}`;
}

/** recordID is a record's id. */
function recordID(record: Record<string, unknown> | undefined): string {
  const id = record?.["id"];
  return typeof id === "string" ? id : "";
}

/** revisionOf is a history entry's revision. */
function revisionOf(entry: Uint8Array): number {
  const decoded = cbor.decode<{ rev?: number }>(entry);
  return typeof decoded?.rev === "number" ? decoded.rev : 0;
}

/** principalOf is who made a history entry. */
function principalOf(entry: Uint8Array): string {
  const decoded = cbor.decode<{ principal?: string }>(entry);
  return decoded?.principal ?? "";
}

/** formatValue is a field's value as a cell. */
function formatValue(value: unknown): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "string" || typeof value === "number") return String(value);
  return JSON.stringify(value);
}
