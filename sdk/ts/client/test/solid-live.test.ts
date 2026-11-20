import { describe, expect, test } from "bun:test";

import { createRoot } from "solid-js";

import { cbor } from "../src/cbor.ts";
import { LiveEventKind, type LiveResponseMessage, type LiveSource } from "../src/live.ts";
import { useLiveQuery } from "../src/solid/live.ts";

/** source is a LiveSource a test feeds one event at a time. */
function source() {
  let push: ((event: LiveResponseMessage) => void) | undefined;
  const handle = {
    requests: [] as unknown[],
    emit(event: LiveResponseMessage) {
      push?.(event);
    },
    live(request: unknown) {
      handle.requests.push(request);
      return {
        [Symbol.asyncIterator]: () => ({
          next: () =>
            new Promise<IteratorResult<LiveResponseMessage>>((resolve) => {
              push = (event) => resolve({ done: false, value: event });
            }),
          return: () => Promise.resolve({ done: true, value: undefined }),
        }),
      };
    },
  };
  return handle as LiveSource & typeof handle;
}

/** tick lets a pushed event reach the query. */
async function tick(): Promise<void> {
  await Bun.sleep(5);
}

describe("useLiveQuery", () => {
  test("reads the records as they change", async () => {
    const stream = source();
    const handle = createRoot((dispose) => {
      const query = useLiveQuery(stream, { collection: "example/notes", retry: -1 });
      return { query, dispose };
    });

    await tick();
    stream.emit({
      kind: LiveEventKind.insert,
      resource: "record://a",
      recordCbor: cbor.encode({ title: "one" }),
    });
    await tick();

    expect(handle.query.status()).toBe("live");
    expect(handle.query.records()).toEqual([
      { resource: "record://a", record: { title: "one" } },
    ]);
    expect(handle.query().records.length).toBe(1);

    handle.dispose();
    expect(handle.query.query.get().status).toBe("live");
    expect(stream.requests.length).toBe(1);
  });
});
