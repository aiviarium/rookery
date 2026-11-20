// A live query as Solid signals: the matching records, the state around them,
// and the lifecycle that closes the stream with the component.
import type { Accessor } from "solid-js";

import {
  LiveQuery,
  type LiveQueryOptions,
  type LiveQueryState,
  type LiveRecord,
  type LiveSource,
} from "../live.ts";
import { fromWatchable, onDispose } from "./signals.ts";

/** LiveQueryHandle is one live query, as a component reads it. */
export interface LiveQueryHandle<T> extends Accessor<LiveQueryState<T>> {
  /** query is the live query itself, for its change events and its restart. */
  readonly query: LiveQuery<T>;
  /** records are the matching records. */
  records: Accessor<readonly LiveRecord<T>[]>;
  /** status is loading, live or error. */
  status: Accessor<"loading" | "live" | "error">;
  /** restart drops the snapshot and asks again, as a space change needs. */
  restart(): void;
}

/**
 * useLiveQuery runs a live query for as long as its owner lives. Pass the space
 * the caller is in — `space: rookery.space()` — and call `restart()` when it
 * changes.
 */
export function useLiveQuery<T = unknown>(
  source: LiveSource,
  options: LiveQueryOptions<T>,
): LiveQueryHandle<T> {
  const query = new LiveQuery<T>(source, options);
  const state = fromWatchable(query);
  const handle = state as unknown as LiveQueryHandle<T>;
  Object.defineProperty(handle, "query", { value: query, enumerable: true });
  // A derived read is a function, not a memo: reading the state signal in it
  // tracks whoever reads it, and there is no value to cache.
  handle.records = () => state().records;
  handle.status = () => state().status;
  handle.restart = () => query.restart();

  query.start();
  onDispose(() => query.close());
  return handle;
}
