// Event subscriptions over a Rookery Connection. Re-subscribes automatically on reconnect because
// the server drops per-connection subscriptions when a WebSocket closes.
//
// Events are at most once, and a connection too slow for a burst loses what does not fit instead of
// closing. The server then sends `events.dropped` with how many: `onDropped` says so, and the
// subscriber reads again what it shows. Nothing replays the lost events, so the only cure is a
// refresh; subscribing again would only restart the same subscription.
import type { EventHandler, EventSubscriptionOptions } from "@rookery/sdk/runtime/client"
import { type Disposable, disposable } from "@rookery/sdk/types"
import { wireCall } from "@rookery/sdk/wire"
import type { EventNotice, EventsDropped } from "./generated"
import type { Connection } from "./ws"

export type { EventHandler, EventSubscriptionOptions }

interface Sub {
  names: string[]
  handler: EventHandler
  onDropped?: (dropped: number) => void
  id?: string
}

export interface EventBus {
  /**
   * Subscribes to the events named by `names`: each `<plugin>/<name>`, or a prefix ending in `*` such
   * as `fleet/*`. Run `onDropped` when the connection lost events of the subscription, and read
   * again what the subscriber shows.
   */
  subscribe(
    names: string | readonly string[],
    handler: EventHandler,
    opts?: EventSubscriptionOptions,
  ): Promise<Disposable>
  dispose(): void
}

/** The names of a subscription as the server takes them: one to 100, each at most 300 characters. */
function namesOf(names: string | readonly string[]): string[] {
  return typeof names === "string" ? [names] : [...names]
}

export function createEventBus(conn: Connection): EventBus {
  const subs = new Set<Sub>()
  const byId = new Map<string, Sub>()

  async function register(s: Sub) {
    const { subscription } = await wireCall(conn, "events.subscribe", { names: s.names })
    if (!subs.has(s)) {
      wireCall(conn, "events.unsubscribe", { subscription }).catch(() => {})
      return
    }
    s.id = subscription
    byId.set(subscription, s)
  }

  const offEvent = conn.on("events.event", (e: EventNotice) => {
    const s = byId.get(e.subscription)
    if (!s) return
    try {
      s.handler(e.payload, e.name)
    } catch (err) {
      console.error(`event handler for ${e.name} failed`, err)
    }
  })

  // The connection had no room for events of the subscription. What was dropped is gone, so the
  // subscriber shows something stale until it reads again.
  const offDropped = conn.on("events.dropped", (e: EventsDropped) => {
    const s = byId.get(e.subscription)
    if (!s) return
    if (!s.onDropped) {
      console.warn(`${e.dropped} events of ${s.names.join(", ")} were dropped; nothing shows them`)
      return
    }
    try {
      s.onDropped(e.dropped)
    } catch (err) {
      console.error(`event drop handler for ${s.names.join(", ")} failed`, err)
    }
  })

  const offReconnect = conn.onReconnect(() => {
    byId.clear()
    for (const s of subs) {
      s.id = undefined
      register(s).catch(() => {})
    }
  })

  return {
    async subscribe(names, handler, opts = {}) {
      const s: Sub = {
        names: namesOf(names),
        handler,
        ...(opts.onDropped ? { onDropped: opts.onDropped } : {}),
      }
      subs.add(s)
      await register(s)
      return disposable(() => {
        subs.delete(s)
        if (s.id) {
          byId.delete(s.id)
          wireCall(conn, "events.unsubscribe", { subscription: s.id }).catch(() => {})
        }
      })
    },
    dispose() {
      offEvent()
      offDropped()
      offReconnect()
      for (const s of subs) {
        if (s.id) wireCall(conn, "events.unsubscribe", { subscription: s.id }).catch(() => {})
      }
      subs.clear()
      byId.clear()
    },
  }
}
