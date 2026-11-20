import { describe, expect, test } from "bun:test";

import { cbor } from "../src/cbor.ts";
import {
  LiveEventKind,
  LiveQuery,
  type LiveResponseMessage,
  type LiveSource,
} from "../src/live.ts";

/** source is a LiveSource a test feeds one event at a time. */
function source(): LiveSource & {
  emit(event: LiveResponseMessage): void;
  end(): void;
  requests: unknown[];
  closed: boolean;
} {
  let push: ((event: LiveResponseMessage) => void) | undefined;
  let finish: (() => void) | undefined;
  const handle = {
    requests: [] as unknown[],
    closed: false,
    emit(event: LiveResponseMessage) {
      push?.(event);
    },
    end() {
      finish?.();
    },
    live(request: unknown) {
      handle.requests.push(request);
      return {
        [Symbol.asyncIterator]() {
          return {
            next: () =>
              new Promise<IteratorResult<LiveResponseMessage>>((resolve) => {
                push = (event) => resolve({ done: false, value: event });
                finish = () => resolve({ done: true, value: undefined });
              }),
            return: () =>
              new Promise<IteratorResult<LiveResponseMessage>>((resolve) => {
                handle.closed = true;
                resolve({ done: true, value: undefined });
              }),
          };
        },
      };
    },
  };
  return handle as LiveSource & typeof handle;
}

/** record is one event's CBOR. */
function record(fields: Record<string, unknown>): Uint8Array {
  return cbor.encode(fields);
}

describe("LiveQuery", () => {
  test("keeps a keyed snapshot through inserts, updates and removals", async () => {
    const stream = source();
    const query = new LiveQuery<{ title: string }>(stream, {
      collection: "example/notes",
      filter: { done: false },
      retry: -1,
    });
    const changes: string[] = [];
    query.onChange((change) => changes.push(change.kind));

    query.start();
    await Bun.sleep(1);
    stream.emit({ kind: LiveEventKind.insert, resource: "record://a", recordCbor: record({ title: "one" }) });
    await Bun.sleep(1);
    stream.emit({ kind: LiveEventKind.insert, resource: "record://b", recordCbor: record({ title: "two" }) });
    await Bun.sleep(1);
    stream.emit({ kind: LiveEventKind.update, resource: "record://b", recordCbor: record({ title: "two!" }) });
    await Bun.sleep(1);
    stream.emit({ kind: LiveEventKind.remove, resource: "record://a", recordCbor: new Uint8Array() });
    await Bun.sleep(1);

    expect(query.get().status).toBe("live");
    expect(query.get().records).toEqual([{ resource: "record://b", record: { title: "two!" } }]);
    expect(changes).toEqual(["insert", "insert", "update", "remove"]);
    expect(stream.requests[0]).toMatchObject({ collection: "example/notes", limit: 0, space: "" });
    expect(cbor.decode((stream.requests[0] as { filterCbor: Uint8Array }).filterCbor)).toEqual({
      done: false,
    });
    query.close();
  });

  test("replaces the snapshot on a resync", async () => {
    const stream = source();
    const query = new LiveQuery<{ title: string }>(stream, { collection: "example/notes", retry: -1 });
    query.start();
    await Bun.sleep(1);
    stream.emit({ kind: LiveEventKind.insert, resource: "record://a", recordCbor: record({ title: "one" }) });
    await Bun.sleep(1);
    stream.emit({ kind: LiveEventKind.resync, resource: "", recordCbor: new Uint8Array() });
    await Bun.sleep(1);
    expect(query.get().records).toEqual([]);
    stream.emit({ kind: LiveEventKind.insert, resource: "record://c", recordCbor: record({ title: "three" }) });
    await Bun.sleep(1);
    expect(query.get().records).toEqual([{ resource: "record://c", record: { title: "three" } }]);
    query.close();
  });

  test("reconnects when the stream ends", async () => {
    const stream = source();
    const query = new LiveQuery(stream, { collection: "example/notes", retry: 0 });
    query.start();
    await Bun.sleep(5);
    stream.end();
    await Bun.sleep(300);
    expect(stream.requests.length).toBe(2);
    query.close();
  });

  test("stops reconnecting once it is closed", async () => {
    const stream = source();
    const query = new LiveQuery(stream, { collection: "example/notes", retry: 0 });
    query.start();
    await Bun.sleep(5);
    query.close();
    stream.end();
    await Bun.sleep(300);
    expect(stream.requests.length).toBe(1);
  });
});
