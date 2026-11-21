// The datastore: the collections plugins declare, their records, and a record's
// history. The collection and the record are in the address; a collection's
// records follow its live query while the page is open.
import { For, Show, createMemo } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import { cbor } from "@rookery/client";
import { useLiveQuery } from "@rookery/client/solid";
import type { Collection } from "@rookery/proto";
import {
  Badge,
  Card,
  CodeBlock,
  DataTable,
  type DataColumn,
  EmptyState,
  ListRow,
  MetaLine,
  RowList,
  Skeleton,
  State,
} from "@aiviarium/nocturne";

import { createLive } from "../../parts/data.ts";
import { errorMessage } from "../../parts/errors.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";

/** DataPage browses the collections the caller may read. */
export function DataPage(): JSX.Element {
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ collection?: string; record?: string }>();
  const collections = createLive(() => shell.rookery.data.listCollections({ space: shell.spaces.current() }), { space: true });
  const rows = () => collections()?.collections ?? [];
  const chosen = createMemo(() => rows().find((one) => keyOf(one) === search.collection));

  return (
    <Show
      when={chosen()}
      keyed
      fallback={
        <View
          id="datastore"
          crumbs={[{ label: "Datastore" }]}
          error={collections.error() === undefined ? undefined : `The collections could not be read. ${errorMessage(collections.error())}`}
          actions={[{ id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: () => void collections.refresh().catch(() => undefined) }]}
        >
          <Show when={collections()} fallback={<Skeleton width="50%" />}>
            <Show
              when={rows().length > 0}
              fallback={
                <Card>
                  <EmptyState
                    icon="layers"
                    title="No collections to read"
                    description="Plugins declare collections in their manifests. Records belong to a space or to the plugin."
                  />
                </Card>
              }
            >
              <Card padding="none">
                <RowList label="Collections">
                  <For each={rows()} keyed={(one) => keyOf(one)}>
                    {(one) => (
                      <ListRow
                        icon="layers"
                        title={<span class="nc-mono">{keyOf(one())}</span>}
                        label={keyOf(one())}
                        secondary={one().fields.join(", ")}
                        meta={
                          <span class="nc-row nc-row--tight">
                            <Badge tone="outline">{one().scope}</Badge>
                            <Show when={one().history}>
                              <Badge tone="info">history</Badge>
                            </Show>
                          </span>
                        }
                        onClick={() => setSearch({ collection: keyOf(one()), record: undefined })}
                      />
                    )}
                  </For>
                </RowList>
              </Card>
            </Show>
          </Show>
        </View>
      }
    >
      {(collection) => (
        <Records
          collection={collection}
          record={search.record ?? ""}
          onRecord={(id) => setSearch({ record: id === "" ? undefined : id })}
          onBack={() => setSearch({ collection: undefined, record: undefined })}
        />
      )}
    </Show>
  );
}

/** Records is one collection's records, kept up to date by its live query, and the chosen record beside them. */
function Records(props: { collection: Collection; record: string; onRecord: (id: string) => void; onBack: () => void }): JSX.Element {
  const shell = useShell();
  const key = keyOf(props.collection);
  const live = useLiveQuery<Record<string, unknown>>(shell.rookery.data, { collection: key, space: shell.spaces.current(), limit: 200 });
  const records = () => live.records().map((row) => row.record ?? {});
  const open = () => records().find((row) => recordID(row) === props.record);
  const fields = createMemo(() => props.collection.fields.filter((field) => field !== "id").slice(0, 6));

  const columns = (): DataColumn<Record<string, unknown>>[] => [
    { id: "id", header: "id", type: "mono", accessor: (row) => recordID(row) },
    ...fields().map((field) => ({ id: field, header: field, accessor: (row: Record<string, unknown>) => formatValue(row[field]) })),
  ];

  return (
    <View
      id="datastore-records"
      crumbs={[{ label: "Datastore", onClick: props.onBack }, { label: key }]}
      context={
        <State status={live.status() === "live" ? "live" : live.status() === "error" ? "error" : "idle"}>
          {`${records().length} records`}
        </State>
      }
      actions={[{ id: "refresh", label: "Ask again", icon: "refresh-cw", onClick: () => live.restart() }]}
      aside={
        <Show when={props.record !== ""}>
          <RecordDetail collection={props.collection} id={props.record} record={open()} />
        </Show>
      }
      asideLabel="Record"
    >
      <Card padding="none">
        <Show when={live.status() !== "loading"} fallback={<Skeleton width="40%" />}>
          <DataTable
            bare
            label={`Records in ${key}`}
            rows={records()}
            columns={columns()}
            getRowId={(row) => recordID(row)}
            onRowClick={(row) => props.onRecord(recordID(row))}
            empty={{
              title: "No records",
              description: "Nothing in this collection, or its records are in a space you are not in.",
            }}
          />
        </Show>
      </Card>
    </View>
  );
}

function RecordDetail(props: { collection: Collection; id: string; record: Record<string, unknown> | undefined }): JSX.Element {
  const shell = useShell();
  const history = createLive(
    () =>
      props.collection.history
        ? shell.rookery.data.history({ collection: keyOf(props.collection), id: props.id, limit: 50 })
        : Promise.resolve(undefined),
    { key: () => props.id },
  );
  return (
    <div class="shell-view">
      <Show when={props.record} fallback={<p class="nc-muted">This record is not among the ones shown.</p>}>
        {(record) => <CodeBlock language="json" title={props.id} lineNumbers={false} code={JSON.stringify(record(), null, 2)} />}
      </Show>
      <Show when={props.collection.history}>
        <Card title="History">
          <Show when={history()} fallback={<Skeleton width="60%" />}>
            {(answer) => (
              <Show when={answer().entriesCbor.length > 0} fallback={<p class="nc-muted">No revisions recorded.</p>}>
                <For each={answer().entriesCbor}>
                  {(entry) => (
                    <div class="nc-stack">
                      <MetaLine items={[{ label: `rev ${revisionOf(entry)}` }, { label: principalOf(entry), mono: true }]} />
                      <CodeBlock
                        language="json"
                        lineNumbers={false}
                        code={JSON.stringify(cbor.decode<Record<string, unknown>>(entry)?.["record"] ?? {}, null, 2)}
                      />
                    </div>
                  )}
                </For>
              </Show>
            )}
          </Show>
        </Card>
      </Show>
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
  return cbor.decode<{ principal?: string }>(entry)?.principal ?? "";
}

/** formatValue is a field's value as a cell. */
function formatValue(value: unknown): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "string" || typeof value === "number") return String(value);
  return JSON.stringify(value);
}
