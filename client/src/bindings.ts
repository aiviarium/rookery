// SolidJS reactive bindings for Rookery data, live queries, RPC calls, events, and binary streams.
import { createLive, type Live, type LiveQuery } from "@rookery/sdk/client"
import {
  type Call,
  type Collection,
  createCollection,
  createRefs,
  type Query,
  type Refs,
  type Row,
} from "@rookery/sdk/data"
import { RookeryError } from "@rookery/sdk/errors"
import type { FilterCollection } from "@rookery/sdk/filter"
import type { ClientStream, StreamOptions } from "@rookery/sdk/runtime/client"
import { disposable } from "@rookery/sdk/types"
import { type Accessor, createSignal, getOwner, onCleanup } from "solid-js"
import type { EventBus, EventHandler } from "./events"
import type { Manifest } from "./generated"
import type { Connection } from "./ws"

export type { Live, LiveQuery }

/** Creates a live query directly over a Rookery `Connection`. */
export function createLiveQuery<T extends Row = Row>(
  conn: Connection,
  target: { plugin: string; collection: string; space?: string | null },
  coll: FilterCollection,
  query: LiveQuery = {},
): Live<T> {
  return createLive<T>(
    {
      call: (method, params) => conn.call(method, params),
      onNotification: (method, fn) => disposable(conn.on(method, fn)),
      onReconnect: (fn) => disposable(conn.onReconnect(fn)),
    },
    target,
    coll,
    query,
  )
}

export interface DataClientCollection<T extends Row = Row> extends Collection<T> {
  live(query?: Omit<Query<T>, "after">): Live<T>
}

export interface DataClient {
  collection<T extends Row = Row>(name: string): DataClientCollection<T>
  readonly refs: Refs
}

/** Creates typed collection + live-query bindings for a plugin's collections over a `Connection`. */
export function createDataClient(
  conn: Connection,
  plugin: string,
  manifest: Pick<Manifest, "contributes">,
  opts: { space?: () => string | null | undefined } = {},
): DataClient {
  const spaceId = () => opts.space?.() ?? null
  const call: Call = (method, params) => {
    const sp = spaceId()
    return conn.call(method, { plugin, ...(sp ? { space: sp } : {}), ...params })
  }
  return {
    refs: createRefs(call),
    collection<T extends Row = Row>(name: string): DataClientCollection<T> {
      const def = manifest.contributes.collections.find((c) => c.name === name)
      const base = createCollection<T>(call, name, (n) =>
        manifest.contributes.collections.find((c) => c.name === n),
      )
      return {
        ...base,
        live(query: Omit<Query<T>, "after"> = {}) {
          if (!def) throw new RookeryError("NOT_FOUND", `plugin ${plugin} declares no collection ${name}`)
          return createLiveQuery<T>(
            conn,
            { plugin, collection: name, space: spaceId() },
            def,
            query as LiveQuery,
          )
        },
      }
    },
  }
}

export type StreamBindingState = "opening" | "open" | "closed" | "error"

export interface CreateStreamOptions extends StreamOptions {
  onData?: (chunk: Uint8Array) => void
  onEnd?: (error?: { code: string; message: string; details?: unknown }) => void
}

export interface StreamBinding<R = any> {
  readonly state: Accessor<StreamBindingState>
  readonly result: Accessor<R | undefined>
  readonly error: Accessor<RookeryError | undefined>
  write(data: Uint8Array): Promise<void>
  eof(): void
  close(): void
  grant(bytes: number): void
}

/**
 * Opens a binary stream channel over `conn` with SolidJS lifecycle management: exposes reactive
 * state/result/error accessors and closes the stream automatically on `onCleanup`.
 */
export function createStream<R = any>(
  conn: Connection,
  method: string,
  params: Record<string, unknown> = {},
  opts: CreateStreamOptions = {},
): StreamBinding<R> {
  const [state, setState] = createSignal<StreamBindingState>("opening")
  const [result, setResult] = createSignal<R | undefined>(undefined)
  const [error, setError] = createSignal<RookeryError | undefined>(undefined)
  let handle: ClientStream | undefined
  let closed = false

  const openPromise = conn
    .stream(method, params, opts.flow ? { flow: opts.flow } : {})
    .then((s) => {
      if (closed) {
        s.close()
        return undefined
      }
      handle = s
      setResult(() => s.result as R)
      setState("open")
      s.onData = (chunk) => {
        if (!closed) opts.onData?.(chunk)
      }
      s.onEnd = (err) => {
        if (closed) return
        handle = undefined
        if (err) {
          setError(new RookeryError((err.code as any) ?? "INTERNAL", err.message, err.details))
          setState("error")
        } else {
          setState("closed")
        }
        opts.onEnd?.(err)
      }
      return s
    })
    .catch((e) => {
      if (closed) return undefined
      setError(RookeryError.from(e))
      setState("error")
      return undefined
    })

  const close = () => {
    if (closed) return
    closed = true
    handle?.close()
    handle = undefined
    setState("closed")
  }

  if (getOwner()) onCleanup(close)

  return {
    state,
    result,
    error,
    async write(data) {
      const s = handle ?? (await openPromise)
      if (!s || closed) throw new RookeryError("UNAVAILABLE", "the stream is closed")
      await s.write(data)
    },
    eof() {
      handle?.eof()
    },
    close,
    grant(bytes) {
      handle?.grant?.(bytes)
    },
  }
}

/** Subscribes to a topic on `bus` and automatically disposes the subscription on `onCleanup`. */
export function createEventSubscription(bus: EventBus, topic: string, handler: EventHandler) {
  let disposed = false
  let disposeSub: (() => void) | undefined

  void bus
    .subscribe(topic, (payload, t) => {
      if (!disposed) handler(payload, t)
    })
    .then((d) => {
      if (disposed) d.dispose()
      else disposeSub = () => d.dispose()
    })
    .catch(() => {})

  const dispose = () => {
    if (disposed) return
    disposed = true
    disposeSub?.()
  }
  if (getOwner()) onCleanup(dispose)
  return { dispose }
}

export interface RpcResource<T> {
  (): T | undefined
  readonly loading: Accessor<boolean>
  readonly error: Accessor<RookeryError | undefined>
  refetch(): Promise<T | undefined>
}

export interface RpcResourceOptions {
  /** Re-run the RPC automatically when the connection reconnects. Default `true`. */
  refetchOnReconnect?: boolean
}

/** Reactive SolidJS resource backed by a JSON-RPC call over `conn`. */
export function createRpcResource<T>(
  conn: Connection,
  method: string,
  params: Record<string, unknown> | (() => Record<string, unknown>) = {},
  opts: RpcResourceOptions = {},
): RpcResource<T> {
  const [data, setData] = createSignal<T | undefined>(undefined)
  const [loading, setLoading] = createSignal(true)
  const [error, setError] = createSignal<RookeryError | undefined>(undefined)
  let seq = 0

  const refetch = async (): Promise<T | undefined> => {
    const token = ++seq
    setLoading(true)
    setError(undefined)
    try {
      const p = typeof params === "function" ? params() : params
      const res = await conn.call<T>(method, p)
      if (token === seq) {
        setData(() => res)
        setLoading(false)
      }
      return res
    } catch (e) {
      if (token === seq) {
        setError(RookeryError.from(e))
        setLoading(false)
      }
      return undefined
    }
  }

  let offReconnect: (() => void) | undefined
  if (opts.refetchOnReconnect !== false) {
    offReconnect = conn.onReconnect(() => {
      void refetch()
    })
  }
  if (getOwner()) onCleanup(() => offReconnect?.())

  void refetch()

  const accessor = (() => data()) as RpcResource<T>
  Object.defineProperties(accessor, {
    loading: { value: loading },
    error: { value: error },
    refetch: { value: refetch },
  })
  return accessor
}
