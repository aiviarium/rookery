// How a page reads the server: a query that asks again when what it shows changes.
//
// A page names the events that change its data and, when it reads a route
// parameter, the key: the query asks again when the key changes or when one of
// the events arrives. A page in a space is drawn afresh in another space, so
// it never follows the space itself.
import { createEffect, onSettled } from "solid-js";
import { type Query, createQuery } from "@rookery/client/solid";

import { useShell } from "../shell.tsx";

/** LiveOptions say when a live query asks again. */
export interface LiveOptions {
  /** events are the event name prefixes that change the data, such as "rookery/hosts.". */
  readonly events?: readonly string[];
  /** key is what the call depends on, such as a host's name from the route. */
  readonly key?: () => unknown;
}

/** createLive is createQuery that follows its key, its events and the space. */
export function createLive<T>(run: () => Promise<T>, options: LiveOptions = {}): Query<T> {
  const shell = useShell();
  const query = createQuery(run);
  const again = () => void query.refresh().catch(() => undefined);

  const key = options.key;
  if (key !== undefined) {
    createEffect(key, (_next, previous) => {
      if (previous !== undefined) again();
    });
  }
  const events = options.events ?? [];
  if (events.length > 0) {
    onSettled(() => {
      const stops = events.map((prefix) => shell.connection.on(prefix, again));
      return () => {
        for (const stop of stops) stop();
      };
    });
  }
  return query;
}
