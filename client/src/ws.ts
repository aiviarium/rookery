// The client's connection to the Rookery server: JSON-RPC 2.0 over /ws, reconnect with backoff,
// notifications, and binary stream channels (4-byte big-endian stream id + payload, credit in streams.credit).
import { RookeryError } from "@rookery/sdk/errors"
import type { ClientStream, StreamOptions } from "@rookery/sdk/runtime/client"
import type { WireMethod, WireParams } from "@rookery/sdk/wire"
import { type Accessor, createEffect, createSignal, getOwner, on, onCleanup } from "solid-js"
import type { StreamOpened } from "./generated"

export type ConnState = "connecting" | "open" | "closed"
export type ConnectionPhase = "ok" | "reconnecting" | "offline" | "back"

export type { ClientStream, StreamOptions }

/** Default initial send window (256 KiB) per binary stream channel. */
export const INITIAL_STREAM_WINDOW = 256 * 1024
/** Maximum single binary frame payload size (64 KiB). */
export const MAX_STREAM_CHUNK = 64 * 1024

/** A blip shorter than this is not surfaced to the user. */
export const GRACE_MS = 1500
/** Still disconnected after this long: show "Offline". */
export const OFFLINE_MS = 8000
/** How long "Back online" remains visible after reconnecting. */
export const BACK_MS = 2800

export interface ConnectionOptions {
  /** WebSocket endpoint URL (e.g. `ws://127.0.0.1:4790/ws`). */
  url: string
  /** Called when the server rejects auth (UNAUTHENTICATED or 401 on session check). */
  onSignedOut?: () => void
  /**
   * Checks whether the session is still valid after a socket closes. Return `false` if signed out.
   * Defaults to `GET /auth/session` on the same origin when running in a browser.
   */
  checkSession?: () => Promise<boolean>
  /** Whether to reconnect automatically after unexpected closes. Default `true`. */
  reconnect?: boolean
  /** Initial reconnect delay in ms (doubles each attempt up to `maxDelayMs`). Default 500. */
  initialDelayMs?: number
  /** Maximum reconnect delay in ms. Default 8000. */
  maxDelayMs?: number
  /** Timeout for `call()` waiting for the socket to open, in ms. Default 15000. */
  callTimeoutMs?: number
  /** Initial send window in bytes for opened streams. Default 256 KiB. */
  initialWindow?: number
  /** Custom WebSocket constructor (defaults to `globalThis.WebSocket`). */
  WebSocketImpl?: typeof WebSocket
}

interface Pending {
  resolve: (v: unknown) => void
  reject: (e: unknown) => void
}

interface StreamState {
  stream: ClientStream
  credit: number
  waiters: Array<() => void>
  manual: boolean
  held?: Uint8Array[]
}

/** Derives the `/ws` URL from a browser `Location`-like object. */
export function wsUrlFromLocation(loc: Pick<Location, "protocol" | "host"> = location): string {
  const scheme = loc.protocol === "https:" ? "wss" : "ws"
  return `${scheme}://${loc.host}/ws`
}

async function defaultCheckSession(): Promise<boolean> {
  if (typeof fetch !== "function" || typeof location === "undefined") return true
  try {
    const res = await fetch("/auth/session")
    return res.status !== 401
  } catch {
    return true
  }
}

export class Connection {
  readonly state: Accessor<ConnState>
  private readonly setState: (s: ConnState) => void
  readonly retryAt: Accessor<number | null>
  private readonly setRetryAt: (t: number | null) => void

  private readonly url: string
  private readonly onSignedOut: () => void
  private readonly checkSession: () => Promise<boolean>
  private readonly shouldReconnect: boolean
  private readonly initialDelayMs: number
  private readonly maxDelayMs: number
  private readonly callTimeoutMs: number
  private readonly initialWindow: number
  private readonly WebSocketImpl: typeof WebSocket

  private ws?: WebSocket
  private next = 1
  private readonly pending = new Map<number, Pending>()
  private readonly listeners = new Map<string, Set<(params: any) => void>>()
  private readonly openHooks = new Set<() => void>()
  private readonly streams = new Map<number, StreamState>()
  /** Frames and ends of streams that arrived before the opening RPC result resolved. */
  private readonly early = new Map<number, { at: number; frames: Uint8Array[]; end?: { error?: any } }>()
  private attempt = 0
  private stopped = false
  private retryTimer?: ReturnType<typeof setTimeout>

  constructor(options: ConnectionOptions | string, onSignedOut?: () => void) {
    const opts: ConnectionOptions = typeof options === "string" ? { url: options, onSignedOut } : options
    this.url = opts.url
    this.onSignedOut = opts.onSignedOut ?? (() => {})
    this.checkSession = opts.checkSession ?? defaultCheckSession
    this.shouldReconnect = opts.reconnect ?? true
    this.initialDelayMs = opts.initialDelayMs ?? 500
    this.maxDelayMs = opts.maxDelayMs ?? 8000
    this.callTimeoutMs = opts.callTimeoutMs ?? 15_000
    this.initialWindow = opts.initialWindow ?? INITIAL_STREAM_WINDOW
    this.WebSocketImpl = opts.WebSocketImpl ?? WebSocket
    ;[this.state, this.setState] = createSignal<ConnState>("closed")
    ;[this.retryAt, this.setRetryAt] = createSignal<number | null>(null)
  }

  connect() {
    clearTimeout(this.retryTimer)
    this.retryTimer = undefined
    this.stopped = false
    this.setState("connecting")
    const ws = new this.WebSocketImpl(this.url)
    ws.binaryType = "arraybuffer"
    this.ws = ws
    ws.onopen = () => {
      if (this.ws !== ws) return
      this.attempt = 0
      this.setRetryAt(null)
      this.setState("open")
      for (const h of this.openHooks) h()
    }
    ws.onmessage = (ev) => {
      if (this.ws !== ws) return
      if (typeof ev.data === "string") {
        this.text(ev.data)
      } else if (ev.data instanceof ArrayBuffer) {
        this.binary(ev.data)
      } else if (ArrayBuffer.isView(ev.data)) {
        const view = ev.data as ArrayBufferView
        const copy = new Uint8Array(view.byteLength)
        copy.set(new Uint8Array(view.buffer, view.byteOffset, view.byteLength))
        this.binary(copy.buffer)
      } else if (typeof Blob !== "undefined" && ev.data instanceof Blob) {
        void ev.data.arrayBuffer().then((buf) => {
          if (this.ws === ws) this.binary(buf)
        })
      }
    }
    ws.onclose = () => {
      if (this.ws !== ws) return
      void this.closed()
    }
    ws.onerror = () => {
      // onclose fires right after onerror and handles state transition and reconnect.
    }
  }

  stop() {
    this.stopped = true
    clearTimeout(this.retryTimer)
    this.retryTimer = undefined
    this.setRetryAt(null)
    const ws = this.ws
    this.ws = undefined
    if (ws) {
      ws.onopen = null
      ws.onmessage = null
      ws.onclose = null
      ws.onerror = null
      ws.close()
    }
    this.failInFlight()
    this.setState("closed")
  }

  private failInFlight() {
    for (const p of this.pending.values())
      p.reject(new RookeryError("UNAVAILABLE", "the connection to the server closed"))
    this.pending.clear()
    for (const s of this.streams.values()) {
      for (const w of s.waiters.splice(0)) w()
      s.stream.onEnd({ code: "UNAVAILABLE", message: "connection closed" })
    }
    this.streams.clear()
  }

  private async closed() {
    this.ws = undefined
    this.setState("closed")
    this.failInFlight()
    if (this.stopped) return
    const valid = await this.checkSession().catch(() => true)
    if (!valid) {
      this.stopped = true
      this.onSignedOut()
      return
    }
    if (!this.shouldReconnect || this.stopped) return
    const delay = Math.min(this.maxDelayMs, this.initialDelayMs * 2 ** this.attempt++)
    this.setRetryAt(Date.now() + delay)
    this.retryTimer = setTimeout(() => {
      this.retryTimer = undefined
      if (!this.stopped) this.connect()
    }, delay)
  }

  /** Attempts to reconnect immediately instead of waiting for the backoff timer. */
  retryNow() {
    if (this.state() !== "closed" || this.stopped) return
    clearTimeout(this.retryTimer)
    this.retryTimer = undefined
    this.attempt = 0
    this.connect()
  }

  /** Runs on every (re)connect, and immediately if already open. */
  onOpen(fn: () => void): () => void {
    this.openHooks.add(fn)
    if (this.state() === "open") fn()
    return () => {
      this.openHooks.delete(fn)
    }
  }

  /** Runs on every reconnect after the first open. */
  onReconnect(fn: () => void): () => void {
    let seenOpen = false
    return this.onOpen(() => {
      if (!seenOpen) {
        seenOpen = true
        return
      }
      fn()
    })
  }

  /** Subscribes to JSON-RPC notifications from the server by method name. */
  on(method: string, fn: (params: any) => void): () => void {
    let set = this.listeners.get(method)
    if (!set) {
      set = new Set()
      this.listeners.set(method, set)
    }
    set.add(fn)
    return () => {
      set.delete(fn)
      if (!set.size) this.listeners.delete(method)
    }
  }

  /** Calls a JSON-RPC method, waiting for the connection if it is being established. */
  async call<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    if (this.state() !== "open") await this.waitOpen()
    const id = this.next++
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject })
      try {
        this.ws!.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }))
      } catch (e) {
        this.pending.delete(id)
        reject(RookeryError.from(e))
      }
    })
  }

  /** Sends a JSON-RPC notification without expecting a response. */
  notify(method: string, params: Record<string, unknown> = {}) {
    if (this.state() === "open") this.ws!.send(JSON.stringify({ jsonrpc: "2.0", method, params }))
  }

  /** Sends a notification of one of the wire's own methods, such as `streams.credit`. */
  private signal<M extends WireMethod>(method: M, params: WireParams<M>) {
    this.notify(method, params as Record<string, unknown>)
  }

  private waitOpen(): Promise<void> {
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        off()
        reject(new RookeryError("UNAVAILABLE", "the server is not reachable"))
      }, this.callTimeoutMs)
      const off = this.onOpen(() => {
        clearTimeout(t)
        off()
        resolve()
      })
    })
  }

  private text(data: string) {
    let msg: any
    try {
      msg = JSON.parse(data)
    } catch {
      return
    }
    if (msg.id !== undefined && msg.method === undefined) {
      const p = this.pending.get(msg.id)
      if (!p) return
      this.pending.delete(msg.id)
      if (msg.error) {
        p.reject(RookeryError.fromRpc(msg.error))
        if (msg.error.data?.code === "UNAUTHENTICATED") this.onSignedOut()
      } else {
        p.resolve(msg.result)
      }
      return
    }
    if (msg.method === "streams.credit") return this.credit(msg.params.stream, msg.params.bytes)
    if (msg.method === "streams.eof" || msg.method === "streams.close") {
      const id = msg.params.stream
      const s = this.streams.get(id)
      if (!s) {
        if (msg.method === "streams.close" || msg.method === "streams.eof") {
          this.earlyOf(id).end = { error: msg.params.error }
        }
        return
      }
      const end = () => {
        if (msg.method === "streams.close") {
          this.streams.delete(id)
          for (const w of s.waiters.splice(0)) w()
        }
        s.stream.onEnd(msg.params.error)
      }
      if (s.held) setTimeout(end, 0)
      else end()
      return
    }
    for (const fn of this.listeners.get(msg.method) ?? []) fn(msg.params)
  }

  private binary(buf: ArrayBuffer) {
    if (buf.byteLength < 4) return
    const id = new DataView(buf).getUint32(0)
    const s = this.streams.get(id)
    const data = new Uint8Array(buf, 4)
    if (!s) {
      this.earlyOf(id).frames.push(data)
      this.signal("streams.credit", { stream: id, bytes: data.byteLength })
      return
    }
    if (s.held) {
      s.held.push(data)
      if (!s.manual) this.signal("streams.credit", { stream: id, bytes: data.byteLength })
      return
    }
    s.stream.onData(data)
    if (!s.manual) this.signal("streams.credit", { stream: id, bytes: data.byteLength })
  }

  private earlyOf(id: number) {
    const now = Date.now()
    for (const [k, v] of this.early) if (now - v.at > 10_000) this.early.delete(k)
    let e = this.early.get(id)
    if (!e) {
      e = { at: now, frames: [] }
      this.early.set(id, e)
    }
    return e
  }

  private credit(id: number, bytes: number) {
    const s = this.streams.get(id)
    if (!s) return
    s.credit += bytes
    for (const w of s.waiters.splice(0)) w()
  }

  /** Calls a method that opens a binary stream channel (`{ stream: number, ... }`). */
  async stream(
    method: string,
    params: Record<string, unknown> = {},
    opts: StreamOptions = {},
  ): Promise<ClientStream> {
    const result = await this.call<StreamOpened>(method, params)
    const id = result.stream
    const state: StreamState = {
      stream: undefined as unknown as ClientStream,
      credit: this.initialWindow,
      waiters: [],
      manual: opts.flow === "manual",
    }
    const conn = this
    const stream: ClientStream = {
      id,
      result,
      async write(data) {
        for (let off = 0; off < data.byteLength; ) {
          while (state.credit <= 0) {
            if (!conn.streams.has(id)) throw new RookeryError("UNAVAILABLE", "the stream is closed")
            await new Promise<void>((r) => state.waiters.push(r))
          }
          if (!conn.streams.has(id)) throw new RookeryError("UNAVAILABLE", "the stream is closed")
          const n = Math.min(state.credit, MAX_STREAM_CHUNK, data.byteLength - off)
          const frame = new Uint8Array(4 + n)
          new DataView(frame.buffer).setUint32(0, id)
          frame.set(data.subarray(off, off + n), 4)
          conn.ws?.send(frame)
          state.credit -= n
          off += n
        }
      },
      eof: () => conn.signal("streams.eof", { stream: id }),
      close: () => {
        conn.streams.delete(id)
        for (const w of state.waiters.splice(0)) w()
        conn.signal("streams.close", { stream: id })
      },
      grant: (bytes) => {
        if (bytes > 0 && conn.streams.has(id)) conn.signal("streams.credit", { stream: id, bytes })
      },
      onData: () => {},
      onEnd: () => {},
    }
    state.stream = stream
    this.streams.set(id, state)
    const early = this.early.get(id)
    if (early) {
      this.early.delete(id)
      state.held = early.frames
      setTimeout(() => {
        const held = state.held ?? []
        state.held = undefined
        for (const f of held) stream.onData(f)
        if (early.end) {
          this.streams.delete(id)
          stream.onEnd(early.end.error)
        }
      }, 0)
    }
    return stream
  }
}

export interface ConnectionPhaseTimings {
  graceMs?: number
  offlineMs?: number
  backMs?: number
}

/**
 * Tracks the user-facing connection phase: silent while healthy or across short blips, then
 * `"reconnecting"`, `"offline"`, and briefly `"back"` once reconnected.
 */
export function createConnectionPhase(
  conn: Connection,
  timings: ConnectionPhaseTimings = {},
): Accessor<ConnectionPhase> {
  const graceMs = timings.graceMs ?? GRACE_MS
  const offlineMs = timings.offlineMs ?? OFFLINE_MS
  const backMs = timings.backMs ?? BACK_MS
  const [phase, setPhase] = createSignal<ConnectionPhase>("ok")
  let down = false
  let hasOpened = conn.state() === "open"
  let grace: ReturnType<typeof setTimeout> | undefined
  let offline: ReturnType<typeof setTimeout> | undefined
  let back: ReturnType<typeof setTimeout> | undefined

  const clearTimers = () => {
    clearTimeout(grace)
    clearTimeout(offline)
    clearTimeout(back)
    grace = undefined
    offline = undefined
    back = undefined
  }

  createEffect(
    on(conn.state, (state) => {
      if (state === "open") {
        clearTimeout(grace)
        clearTimeout(offline)
        grace = undefined
        offline = undefined
        const wasShown = phase() === "reconnecting" || phase() === "offline"
        down = false
        hasOpened = true
        if (wasShown) {
          setPhase("back")
          clearTimeout(back)
          back = setTimeout(() => setPhase("ok"), backMs)
        }
      } else if (hasOpened && !down) {
        down = true
        clearTimeout(back)
        back = undefined
        if (phase() === "back") setPhase("ok")
        grace = setTimeout(() => setPhase("reconnecting"), graceMs)
        offline = setTimeout(() => setPhase("offline"), offlineMs)
      }
    }),
  )

  if (getOwner()) onCleanup(clearTimers)
  return phase
}
