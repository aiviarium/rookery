// Whether the shell is talking to the server.
//
// The events stream is the shell's heartbeat: it is one long-lived call, and
// while it is open the edge answers. It also tells the shell what changed,
// which pages use to ask again. A stream that ends reconnects with backoff, and
// a browser that says it is offline waits for its own `online` event.
import { type Accessor, createEffect, createSignal } from "solid-js";
import type { Rookery } from "@rookery/client";
import type { Event } from "@rookery/proto";

/** ConnectionStatus is where the shell's link to the server is. */
export type ConnectionStatus = "connecting" | "online" | "offline";

/** EventListener is what a page does when an event matches. */
export type EventListener = (event: Event) => void;

/** Connection is the events stream, and the state a status bar draws. */
export interface Connection {
  /** status is connecting, online or offline. */
  readonly status: Accessor<ConnectionStatus>;
  /**
   * on hears events whose name starts with a prefix. It returns what stops
   * listening, which a page calls when it is disposed.
   */
  on(prefix: string, listener: EventListener): () => void;
}

/** RETRY_MIN and RETRY_MAX bound the wait between reconnects, in milliseconds. */
const RETRY_MIN = 250;
const RETRY_MAX = 5000;

/**
 * createConnection follows the events stream while someone is signed in, and
 * reports the link's state.
 */
export function createConnection(rookery: Rookery, signedIn: Accessor<boolean>): Connection {
  const [status, setStatus] = createSignal<ConnectionStatus>("connecting");
  const listeners = new Set<{ prefix: string; listener: EventListener }>();

  const hear = (event: Event) => {
    for (const entry of listeners) {
      if (event.name.startsWith(entry.prefix)) entry.listener(event);
    }
  };

  createEffect(
    () => signedIn(),
    (signed) => {
      if (!signed) {
        setStatus("connecting");
        return;
      }

      const controller = new AbortController();
      let timer: ReturnType<typeof setTimeout> | undefined;
      let attempt = 0;

      const wait = (milliseconds: number) =>
        new Promise<void>((resolve) => {
          timer = setTimeout(resolve, milliseconds);
          controller.signal.addEventListener("abort", () => resolve(), { once: true });
        });

      const follow = async () => {
        while (!controller.signal.aborted) {
          setStatus("connecting");
          try {
            const stream = rookery.events.subscribe({ prefix: "" }, { signal: controller.signal });
            // The stream is open as soon as the call answers, which for a server
            // stream is when its headers arrive: the link is up even while
            // nothing happens.
            setStatus("online");
            for await (const frame of stream) {
              setStatus("online");
              attempt = 0;
              if (frame.event !== undefined) hear(frame.event);
            }
          } catch {
            // A stream that ends for any reason is retried below; the state says
            // the link is down while it is.
          }
          if (controller.signal.aborted) return;
          setStatus("offline");
          attempt += 1;
          await wait(Math.min(RETRY_MAX, RETRY_MIN * 2 ** attempt));
        }
      };
      void follow();

      return () => {
        controller.abort();
        if (timer !== undefined) clearTimeout(timer);
      };
    },
  );

  return {
    status,
    on: (prefix, listener) => {
      const entry = { prefix, listener };
      listeners.add(entry);
      return () => {
        listeners.delete(entry);
      };
    },
  };
}
