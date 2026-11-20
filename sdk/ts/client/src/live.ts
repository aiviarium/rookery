// A live query: the records of a collection that match now, then every change
// that makes one match or stop matching.
//
// DataService.Live is a server stream, and a subscriber that falls behind is
// sent `resync` and then the records that match now. This helper keeps a keyed
// snapshot, replaces it on a resync, and reconnects when the stream ends, so a
// page never shows a half-emptied list.
//
// DataService's proto lands on `go` separately; until then the stream is a
// structural `LiveSource` (the numbers below mirror `LiveEventKind` in
// `proto/rookery/v1/data.proto`), and `createLiveQuery` takes the adapter. When
// the proto lands, `rookery.data` becomes the default source and the numbers
// come from the generated enum.
import { cbor } from "./cbor.ts";
import { type Watchable, watchable } from "./watchable.ts";

/** LiveEventKind is what one event of a live query did. */
export const LiveEventKind = {
  insert: 1,
  update: 2,
  remove: 3,
  resync: 4,
} as const;

/** LiveRequestMessage is the request a live query sends. */
export interface LiveRequestMessage {
  /** collection is "plugin/collection". */
  collection: string;
  /** filterCbor is the filter as CBOR, and empty for every record. */
  filterCbor?: Uint8Array;
  /** sorts order the snapshot. */
  sorts?: { field: string; desc?: boolean }[];
  /** limit bounds the snapshot: 100 when zero, 1000 at most. */
  limit?: number;
  /** space is the caller's current space, for a space-scoped collection. */
  space?: string;
}

/** LiveResponseMessage is one event of a live query, as DataService.Live sends it. */
export interface LiveResponseMessage {
  kind: number;
  resource: string;
  recordCbor: Uint8Array;
}

/** LiveSource is the stream a live query reads. */
export interface LiveSource {
  live(
    request: LiveRequestMessage,
    options: { signal: AbortSignal },
  ): AsyncIterable<LiveResponseMessage>;
}

/** LiveRecord is a record that matches, with its URI. */
export interface LiveRecord<T> {
  /** resource is the record's URI, such as record://example/notes/01h… */
  resource: string;
  /** record is the decoded record. */
  record: T;
}

/** LiveChange is what one event did to a snapshot. */
export type LiveChange<T> =
  | { readonly kind: "insert"; readonly resource: string; readonly record: T }
  | { readonly kind: "update"; readonly resource: string; readonly record: T }
  | { readonly kind: "remove"; readonly resource: string }
  | { readonly kind: "reset" };

/** LiveQueryState is what a page renders from. */
export interface LiveQueryState<T> {
  /** status is loading before the first event, then live, or error. */
  readonly status: "loading" | "live" | "error";
  /** records are the matching records, in the server's order. */
  readonly records: readonly LiveRecord<T>[];
  /** error is why the stream failed, while status is error. */
  readonly error?: Error;
}

/** LiveQueryOptions configures one live query. */
export interface LiveQueryOptions<T = unknown> {
  /** collection is "plugin/collection". */
  collection: string;
  /** filter is the filter as a value, encoded here; empty matches every record. */
  filter?: unknown;
  /** sorts order the snapshot. */
  sorts?: { field: string; desc?: boolean }[];
  /** limit bounds the snapshot. */
  limit?: number;
  /** space is the caller's current space; a query helper adds it. */
  space?: string;
  /** decode reads a record's CBOR; the default decodes it to a plain value. */
  decode?: (record: Uint8Array) => T;
  /**
   * retry is how long to wait before reconnecting after the stream ends, in
   * milliseconds. It doubles per failed attempt up to 5s. Zero means 250ms, and
   * a negative number means not to reconnect.
   */
  retry?: number;
}

/** LiveQuery is one live query, running until it is closed. */
export class LiveQuery<T = unknown> implements Watchable<LiveQueryState<T>> {
  readonly #source: LiveSource;
  readonly #options: LiveQueryOptions<T>;
  readonly #decode: (record: Uint8Array) => T;

  readonly #state: Watchable<LiveQueryState<T>>;
  readonly #records = new Map<string, T>();
  readonly #changes = new Set<(change: LiveChange<T>) => void>();

  #recordsCache: readonly LiveRecord<T>[] | undefined;
  #controller: AbortController | undefined;
  #attempt = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #closed = false;

  constructor(source: LiveSource, options: LiveQueryOptions<T>) {
    this.#source = source;
    this.#options = options;
    this.#decode = options.decode ?? ((record) => cbor.decode<T>(record) as T);
    this.#state = watchable<LiveQueryState<T>>({ status: "loading", records: [] });
  }

  /** get is the state now. */
  get(): LiveQueryState<T> {
    return this.#state.get();
  }

  /** set replaces the state; a page's own store uses it. */
  set(state: LiveQueryState<T>): void {
    this.#state.set(state);
  }

  /** update changes the state from the old one. */
  update(change: (state: LiveQueryState<T>) => LiveQueryState<T>): void {
    this.#state.update(change);
  }

  /** subscribe calls listener now and on every state change. */
  subscribe(listener: (state: LiveQueryState<T>) => void): () => void {
    return this.#state.subscribe(listener);
  }

  /** onChange calls listener for each event, without the whole snapshot. */
  onChange(listener: (change: LiveChange<T>) => void): () => void {
    this.#changes.add(listener);
    return () => {
      this.#changes.delete(listener);
    };
  }

  /** start opens the stream, and does nothing while it is open already. */
  start(): this {
    if (this.#controller !== undefined || this.#closed) return this;
    const controller = new AbortController();
    this.#controller = controller;
    void this.#run(controller.signal);
    return this;
  }

  /** restart drops the snapshot and asks the server again, as a space change needs. */
  restart(): this {
    this.#stopStream();
    this.#records.clear();
    this.#recordsCache = undefined;
    this.#state.set({ status: "loading", records: [] });
    this.#emit({ kind: "reset" });
    return this.start();
  }

  /** close stops the query for good. */
  close(): void {
    this.#closed = true;
    this.#stopStream();
    if (this.#timer !== undefined) {
      clearTimeout(this.#timer);
      this.#timer = undefined;
    }
    this.#changes.clear();
  }

  async #run(signal: AbortSignal): Promise<void> {
    try {
      const request: LiveRequestMessage = {
        collection: this.#options.collection,
        filterCbor:
          this.#options.filter === undefined ? undefined : cbor.encode(this.#options.filter),
        sorts: this.#options.sorts ?? [],
        limit: this.#options.limit ?? 0,
        space: this.#options.space ?? "",
      };
      for await (const event of this.#source.live(request, { signal })) {
        if (signal.aborted) return;
        this.#apply(event);
        if (this.#attempt !== 0) this.#attempt = 0;
      }
      // The stream ended: a leader change or a network drop. Connect again.
      if (!signal.aborted) this.#reconnect();
    } catch (error) {
      if (signal.aborted) return;
      this.#state.set({
        status: this.get().status === "live" ? "live" : "error",
        records: this.get().records,
        error: error instanceof Error ? error : new Error(String(error)),
      });
      this.#reconnect();
    } finally {
      if (this.#controller?.signal === signal) this.#controller = undefined;
    }
  }

  /** #reconnect schedules the next attempt, unless the query is closed. */
  #reconnect(): void {
    if (this.#closed) return;
    const first = this.#options.retry ?? 250;
    if (first < 0) return;
    const wait = Math.min(first === 0 ? 250 : first * 2 ** this.#attempt, 5000);
    this.#attempt += 1;
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      this.start();
    }, wait);
  }

  /** #stopStream cancels the running stream. */
  #stopStream(): void {
    this.#controller?.abort();
    this.#controller = undefined;
  }

  /** #apply folds one event into the snapshot. */
  #apply(event: LiveResponseMessage): void {
    switch (event.kind) {
      case LiveEventKind.insert:
      case LiveEventKind.update: {
        const record = this.#decode(event.recordCbor);
        this.#records.set(event.resource, record);
        this.#publish(event.kind === LiveEventKind.insert ? "insert" : "update", event.resource, record);
        return;
      }
      case LiveEventKind.remove: {
        const had = this.#records.delete(event.resource);
        if (had) this.#publish("remove", event.resource, undefined);
        return;
      }
      case LiveEventKind.resync: {
        // What follows are the records that match now: drop everything and let
        // the inserts after this event fill it in.
        this.#records.clear();
        this.#publish("reset");
        return;
      }
      default:
        // An event kind this client does not know is one a newer server added:
        // the next resync brings the snapshot right again.
        return;
    }
  }

  /** #publish updates the state and tells the listeners. */
  #publish(kind: "insert" | "update" | "remove" | "reset", resource?: string, record?: T): void {
    this.#recordsCache = undefined;
    this.#state.set({ status: "live", records: this.#snapshot() });
    this.#emit(
      kind === "reset"
        ? { kind: "reset" }
        : kind === "remove"
          ? { kind: "remove", resource: resource as string }
          : { kind, resource: resource as string, record: record as T },
    );
  }

  /** #emit tells the change listeners. */
  #emit(change: LiveChange<T>): void {
    for (const listener of [...this.#changes]) listener(change);
  }

  /** #snapshot is the records in insertion order, as a stable array. */
  #snapshot(): readonly LiveRecord<T>[] {
    if (this.#recordsCache === undefined) {
      this.#recordsCache = [...this.#records].map(([resource, record]) => ({ resource, record }));
    }
    return this.#recordsCache;
  }
}
