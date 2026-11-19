// Event subscriptions over a Rookery Connection. Re-subscribes automatically on reconnect because
// the server drops per-connection subscriptions when a WebSocket closes.
import { type Disposable, disposable } from "@rookery/sdk/types"
import type { Event, EventSubscription } from "./generated"
import type { Connection } from "./ws"

export type EventHandler = (payload: unknown, topic: string) => unknown

interface Sub {
  topic: string
  handler: EventHandler
  id?: string
}

export interface EventBus {
  subscribe(topic: string, handler: EventHandler): Promise<Disposable>
  dispose(): void
}

export function createEventBus(conn: Connection): EventBus {
  const subs = new Set<Sub>()
  const byId = new Map<string, Sub>()

  async function register(s: Sub) {
    const { subscription } = await conn.call<EventSubscription>("events.subscribe", { topic: s.topic })
    if (!subs.has(s)) {
      conn.call("events.unsubscribe", { subscription }).catch(() => {})
      return
    }
    s.id = subscription
    byId.set(subscription, s)
  }

  const offEvent = conn.on("events.event", (e: Event) => {
    const s = byId.get(e.subscription)
    if (!s) return
    try {
      s.handler(e.payload, e.topic)
    } catch (err) {
      console.error(`event handler for ${e.topic} failed`, err)
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
    async subscribe(topic, handler) {
      const s: Sub = { topic, handler }
      subs.add(s)
      await register(s)
      return disposable(() => {
        subs.delete(s)
        if (s.id) {
          byId.delete(s.id)
          conn.call("events.unsubscribe", { subscription: s.id }).catch(() => {})
        }
      })
    },
    dispose() {
      offEvent()
      offReconnect()
      for (const s of subs) {
        if (s.id) conn.call("events.unsubscribe", { subscription: s.id }).catch(() => {})
      }
      subs.clear()
      byId.clear()
    },
  }
}
