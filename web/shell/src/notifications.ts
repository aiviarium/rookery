// The notifications a person has, and the ones that arrive while the page is
// open.
//
// The list is one call and the watch is a stream; a notification that arrives is
// put in front of the list unless it is already there.
import { type Accessor, createEffect, createSignal } from "solid-js";
import type { Rookery } from "@rookery/client";
import type { Notification } from "@rookery/proto";

/** Notifications is a person's own notifications. */
export interface Notifications {
  /** items are the newest notifications, unread and read. */
  readonly items: Accessor<readonly Notification[]>;
  /** unread is how many of them are unread. */
  readonly unread: Accessor<number>;
  /** error is why the list could not be read, or the stream ended. */
  readonly error: Accessor<Error | undefined>;
  /** refresh reads the list again. */
  refresh(): Promise<void>;
  /** markRead marks one notification read, or unread again. */
  markRead(id: string, read?: boolean): Promise<void>;
  /** markAllRead marks every unread notification read. */
  markAllRead(): Promise<void>;
}

/** LIST_LIMIT is how many notifications the shell keeps. */
const LIST_LIMIT = 50;

/** createNotifications reads and watches a person's notifications. */
export function createNotifications(rookery: Rookery, signedIn: Accessor<boolean>): Notifications {
  const [items, setItems] = createSignal<readonly Notification[]>([]);
  const [error, setError] = createSignal<Error>();

  const refresh = async () => {
    try {
      const answer = await rookery.notifications.list({ limit: LIST_LIMIT });
      setItems(answer.notifications);
      setError(undefined);
    } catch (failure) {
      setError(asError(failure));
    }
  };

  const receive = (notification: Notification) => {
    setItems((current) => {
      if (current.some((item) => item.id === notification.id)) {
        return current.map((item) => (item.id === notification.id ? notification : item));
      }
      return [notification, ...current].slice(0, LIST_LIMIT);
    });
  };

  const markRead = async (id: string, read = true) => {
    const answer = await rookery.notifications.markRead({ notification: id, read });
    if (answer.notification !== undefined) receive(answer.notification);
  };

  const markAllRead = async () => {
    for (const item of items()) {
      if (!item.read) await markRead(item.id, true);
    }
  };

  createEffect(
    () => signedIn(),
    (signed) => {
      if (!signed) {
        setItems([]);
        return;
      }
      let stopped = false;
      const controller = new AbortController();

      void refresh();
      const follow = async () => {
        let attempt = 0;
        while (!stopped) {
          try {
            const stream = rookery.notifications.watch({}, { signal: controller.signal });
            for await (const frame of stream) {
              attempt = 0;
              if (frame.notification !== undefined) receive(frame.notification);
            }
          } catch (failure) {
            if (stopped) return;
            setError(asError(failure));
          }
          if (stopped) return;
          attempt += 1;
          await wait(Math.min(5000, 250 * 2 ** attempt));
        }
      };
      void follow();

      return () => {
        stopped = true;
        controller.abort();
      };
    },
  );

  return {
    items,
    unread: () => items().filter((item) => !item.read).length,
    error,
    refresh,
    markRead,
    markAllRead,
  };
}

/** wait is a pause a stream's reconnect counts on. */
function wait(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

/** asError is anything thrown, as an Error. */
function asError(failure: unknown): Error {
  return failure instanceof Error ? failure : new Error(String(failure));
}
