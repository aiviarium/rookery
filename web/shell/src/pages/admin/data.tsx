// The datastore: the collections plugins declare, their records, a record's
// history, and the changes that arrive while the page is open.
import { For, Show, createEffect, createMemo, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { createQuery, useLiveQuery } from "@rookery/client/solid";
import type { Collection } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  CodeBlock,
  DataTable,
  type DataColumn,
  EmptyState,
  MetaLine,
  Select,
  Skeleton,
} from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

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
        action={
          <Select
            aria-label="The collection"
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
        <Show when={collections() !== undefined} fallback={<Skeleton width="30%" />}>
          <Show
            when={(collections()?.collections.length ?? 0) > 0}
            fallback={
              <EmptyState
                size="sm"
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
                  <MetaLine items={[{ label: candidate().fields.join(", "), mono: true }]} />
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

  const fields = createMemo(() => props.collection.fields.slice(0, 6));

  const columns = (): DataColumn<Record<string, unknown>>[] => [
    { id: "id", header: "id", type: "mono", accessor: row => recordID(row) },
    ...fields().map(field => ({
      id: field,
      header: field,
      accessor: (row: Record<string, unknown>) => formatValue(row[field]),
    })),
  ];

  return (
    <div class="shell-columns">
      <Card title={key} padding="none" action={<MetaLine items={[`${live.records().length} records, live: ${live.status()}`]} />}>
        <Show
          when={live.status() !== "loading"}
          fallback={<Skeleton width="40%" />}
        >
          <DataTable
            bare
            label={`Records in ${key}`}
            rows={live.records().map(row => row.record ?? {})}
            columns={columns()}
            getRowId={row => recordID(row)}
            onRowClick={row => setOpen(recordID(row))}
            empty={{
              title: "No records",
              description: "Nothing in this collection matches, or the rows are in a space you are not in.",
            }}
            footer={
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
        </Show>
      </Card>

      <Card title="History">
        <Show when={open() !== ""} fallback={<p class="nc-meta">Choose a record to see its revisions.</p>}>
          <Show
            when={(history()?.entriesCbor.length ?? 0) > 0}
            fallback={<Callout tone="info">This collection keeps no history, or the record has one revision.</Callout>}
          >
            <div class="nc-stack">
              <For each={history()?.entriesCbor ?? []}>
                {entry => (
                  <div class="nc-stack">
                    <MetaLine
                      items={[
                        { label: `rev ${revisionOf(entry)}` },
                        { label: principalOf(entry), mono: true },
                      ]}
                    />
                    <CodeBlock
                      language="json"
                      title="record"
                      lineNumbers={false}
                      code={JSON.stringify(cbor.decode<Record<string, unknown>>(entry)?.["record"] ?? {}, null, 2)}
                    />
                  </div>
                )}
              </For>
            </div>
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
