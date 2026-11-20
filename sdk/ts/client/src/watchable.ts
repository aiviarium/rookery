// A value that changes and the listeners that hear each change. It is what the
// client hands a framework before a framework is involved: the Solid bindings
// in `@rookery/client/solid` wrap it in signals, and a plain page subscribes.
export interface Watchable<T> {
  /** get is the value now. */
  get(): T;
  /** set replaces the value and tells every listener. */
  set(value: T): void;
  /** update changes the value from the old one. */
  update(change: (value: T) => T): void;
  /** subscribe calls listener now and on every change, until it is called. */
  subscribe(listener: (value: T) => void): () => void;
}

/** watchable returns a value and the listeners that watch it. */
export function watchable<T>(initial: T): Watchable<T> {
  let value = initial;
  const listeners = new Set<(value: T) => void>();
  return {
    get: () => value,
    set: (next) => {
      value = next;
      for (const listener of [...listeners]) listener(value);
    },
    update: (change) => {
      const next = change(value);
      if (next === value) return;
      value = next;
      for (const listener of [...listeners]) listener(value);
    },
    subscribe: (listener) => {
      listeners.add(listener);
      listener(value);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
