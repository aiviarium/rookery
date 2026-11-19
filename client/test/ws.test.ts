import { afterEach, describe, expect, test } from "bun:test"
import { RookeryError } from "@rookery/sdk/errors"
import { createRoot } from "solid-js"
import { createEventBus } from "../src/events"
import { Connection, createConnectionPhase } from "../src/ws"

interface FakeServer {
  server: ReturnType<typeof Bun.serve>
  wsUrl: string
  sockets: Set<any>
  receivedRpc: Array<{ id?: number; method: string; params: any }>
  receivedBinary: Array<{ streamId: number; payload: Uint8Array }>
  credits: Array<{ stream: number; bytes: number }>
  sendNotification(method: string, params: unknown): void
  sendBinary(streamId: number, payload: Uint8Array): void
  closeAll(code?: number): void
  stop(): void
}

function startFakeWsServer(
  handlers: Record<string, (params: any, ws: any, ctx: FakeServer) => unknown | Promise<unknown>> = {},
): FakeServer {
  const sockets = new Set<any>()
  const receivedRpc: Array<{ id?: number; method: string; params: any }> = []
  const receivedBinary: Array<{ streamId: number; payload: Uint8Array }> = []
  const credits: Array<{ stream: number; bytes: number }> = []

  const ctx: FakeServer = {
    server: null as any,
    wsUrl: "",
    sockets,
    receivedRpc,
    receivedBinary,
    credits,
    sendNotification(method, params) {
      const msg = JSON.stringify({ jsonrpc: "2.0", method, params })
      for (const ws of sockets) ws.send(msg)
    },
    sendBinary(streamId, payload) {
      const frame = new Uint8Array(4 + payload.byteLength)
      new DataView(frame.buffer).setUint32(0, streamId, false)
      frame.set(payload, 4)
      for (const ws of sockets) ws.send(frame)
    },
    closeAll(code = 1012) {
      for (const ws of sockets) ws.close(code, "restart")
    },
    stop() {
      ctx.server.stop(true)
    },
  }

  ctx.server = Bun.serve({
    port: 0,
    fetch(req, server) {
      const url = new URL(req.url)
      if (url.pathname === "/ws") {
        if (server.upgrade(req, { data: undefined })) return undefined
        return new Response("upgrade failed", { status: 400 })
      }
      return new Response("not found", { status: 404 })
    },
    websocket: {
      open(ws) {
        sockets.add(ws)
      },
      close(ws) {
        sockets.delete(ws)
      },
      async message(ws, data) {
        if (typeof data !== "string") {
          const bytes = new Uint8Array(data)
          const streamId = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(0, false)
          receivedBinary.push({ streamId, payload: bytes.slice(4) })
          return
        }
        const msg = JSON.parse(data)
        receivedRpc.push({ id: msg.id, method: msg.method, params: msg.params })
        if (msg.method === "streams.credit") {
          credits.push({ stream: msg.params.stream, bytes: msg.params.bytes })
          return
        }
        const handler = handlers[msg.method]
        if (msg.id === undefined) {
          if (handler) await handler(msg.params, ws, ctx)
          return
        }
        if (!handler) {
          ws.send(
            JSON.stringify({
              jsonrpc: "2.0",
              id: msg.id,
              error: { code: -32601, message: `unknown method ${msg.method}`, data: { code: "NOT_FOUND" } },
            }),
          )
          return
        }
        try {
          const result = await handler(msg.params, ws, ctx)
          ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }))
        } catch (err: any) {
          ws.send(
            JSON.stringify({
              jsonrpc: "2.0",
              id: msg.id,
              error: {
                code: err?.rpcCode ?? -32000,
                message: err?.message ?? String(err),
                data: { code: err?.code ?? "INTERNAL", details: err?.details },
              },
            }),
          )
        }
      },
    },
  })

  ctx.wsUrl = `ws://127.0.0.1:${ctx.server.port}/ws`
  return ctx
}

const cleanup: Array<() => void> = []
afterEach(() => {
  while (cleanup.length) cleanup.pop()!()
})

async function waitFor(fn: () => boolean, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (fn()) return
    await Bun.sleep(10)
  }
  throw new Error("timed out waiting for condition")
}

describe("@rookery/client WebSocket protocol", () => {
  test("JSON-RPC calls, typed errors, UNAUTHENTICATED callback, and notifications", async () => {
    let signedOutCount = 0
    const srv = startFakeWsServer({
      "echo.hello": (p) => ({ greeting: `hi ${p.name}` }),
      "fail.invalid": () => {
        throw new RookeryError("INVALID", "bad param", { field: "name" })
      },
      "fail.auth": () => {
        throw new RookeryError("UNAUTHENTICATED", "session expired")
      },
    })
    cleanup.push(() => srv.stop())

    const conn = new Connection({
      url: srv.wsUrl,
      onSignedOut: () => signedOutCount++,
      checkSession: async () => true,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())
    conn.connect()

    const res = await conn.call<{ greeting: string }>("echo.hello", { name: "rookery" })
    expect(res).toEqual({ greeting: "hi rookery" })

    await expect(conn.call("fail.invalid")).rejects.toMatchObject({
      code: "INVALID",
      message: "bad param",
      details: { field: "name" },
    })

    await expect(conn.call("fail.auth")).rejects.toMatchObject({ code: "UNAUTHENTICATED" })
    expect(signedOutCount).toBe(1)

    const notices: unknown[] = []
    const off = conn.on("demo.notice", (p) => notices.push(p))
    srv.sendNotification("demo.notice", { ok: true })
    await waitFor(() => notices.length === 1)
    expect(notices).toEqual([{ ok: true }])
    off()
  })

  test("binary stream framing, auto credit, manual flow control, early frames, and EOF", async () => {
    let nextStream = 10
    const srv = startFakeWsServer({
      "stream.echo": (_p, _ws, ctx) => {
        const id = nextStream++
        // Send an early frame BEFORE returning the RPC response to test early-frame buffering.
        ctx.sendBinary(id, new TextEncoder().encode("early-"))
        return { stream: id, meta: "ready" }
      },
      "stream.manual": () => ({ stream: nextStream++ }),
    })
    cleanup.push(() => srv.stop())

    const conn = new Connection({
      url: srv.wsUrl,
      checkSession: async () => true,
      initialWindow: 8,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())
    conn.connect()

    const stream = await conn.stream("stream.echo", {})
    expect(stream.result).toEqual({ stream: 10, meta: "ready" })

    const chunks: string[] = []
    let ended = false
    stream.onData = (d) => chunks.push(new TextDecoder().decode(d))
    stream.onEnd = () => {
      ended = true
    }

    // Early frame is delivered once onData is attached; send a second frame + eof.
    srv.sendBinary(10, new TextEncoder().encode("second"))
    await waitFor(() => chunks.join("") === "early-second")

    // Auto credit should have been sent for the received bytes.
    await waitFor(() => srv.credits.some((c) => c.stream === 10))
    const totalCredited = srv.credits.filter((c) => c.stream === 10).reduce((a, c) => a + c.bytes, 0)
    expect(totalCredited).toBe("early-".length + "second".length)

    // Backpressure on send: initialWindow is 8, writing 10 bytes blocks until server grants credit.
    let writeDone = false
    const writePromise = stream.write(new TextEncoder().encode("0123456789")).then(() => {
      writeDone = true
    })
    await Bun.sleep(30)
    expect(writeDone).toBe(false)
    // First 8 bytes should already be transmitted.
    await waitFor(() => srv.receivedBinary.some((b) => b.streamId === 10))
    srv.sendNotification("streams.credit", { stream: 10, bytes: 16 })
    await writePromise
    expect(writeDone).toBe(true)

    await waitFor(
      () =>
        srv.receivedBinary
          .filter((b) => b.streamId === 10)
          .map((b) => new TextDecoder().decode(b.payload))
          .join("") === "0123456789",
    )

    srv.sendNotification("streams.eof", { stream: 10 })
    await waitFor(() => ended)

    // Manual flow control: server receives credit ONLY when stream.grant() is called.
    const manual = await conn.stream("stream.manual", {}, { flow: "manual" })
    const mChunks: Uint8Array[] = []
    manual.onData = (d) => mChunks.push(d)
    srv.sendBinary(manual.id, new Uint8Array([1, 2, 3, 4]))
    await waitFor(() => mChunks.length === 1)
    await Bun.sleep(25)
    expect(srv.credits.filter((c) => c.stream === manual.id)).toHaveLength(0)
    manual.grant?.(4)
    await waitFor(() => srv.credits.some((c) => c.stream === manual.id && c.bytes === 4))
    manual.close()
  })

  test("reconnect restores connection and EventBus resubscribes topics", async () => {
    const subscribedTopics: Array<{ topic: string; subscription: string }> = []
    let subSeq = 0
    const srv = startFakeWsServer({
      "events.subscribe": (p) => {
        const subscription = `esub-${++subSeq}`
        subscribedTopics.push({ topic: p.topic, subscription })
        return { subscription }
      },
      "events.unsubscribe": () => ({}),
    })
    cleanup.push(() => srv.stop())

    const conn = new Connection({
      url: srv.wsUrl,
      checkSession: async () => true,
      initialDelayMs: 20,
      maxDelayMs: 40,
    })
    cleanup.push(() => conn.stop())
    const bus = createEventBus(conn)
    cleanup.push(() => bus.dispose())

    const received: unknown[] = []
    const subPromise = bus.subscribe("hosts.changed", (p) => received.push(p))
    conn.connect()
    await subPromise

    expect(subscribedTopics).toHaveLength(1)
    srv.sendNotification("events.event", {
      subscription: subscribedTopics[0]!.subscription,
      topic: "hosts.changed",
      payload: { n: 1 },
    })
    await waitFor(() => received.length === 1)

    // Drop the socket from the server side; client should reconnect and resubscribe.
    srv.closeAll(1012)
    await waitFor(() => subscribedTopics.length >= 2)
    // Allow client subscribe response to register in byId.
    await Bun.sleep(20)
    srv.sendNotification("events.event", {
      subscription: subscribedTopics[1]!.subscription,
      topic: "hosts.changed",
      payload: { n: 2 },
    })
    await waitFor(() => received.length === 2)
    expect(received).toEqual([{ n: 1 }, { n: 2 }])
  })

  test("createConnectionPhase transitions ok -> reconnecting -> offline -> back -> ok", async () => {
    const srv = startFakeWsServer({
      ping: () => "pong",
    })
    cleanup.push(() => srv.stop())

    const conn = new Connection({
      url: srv.wsUrl,
      checkSession: async () => true,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())

    let disposeRoot: (() => void) | undefined
    const phases: string[] = []
    let phaseAccessor!: () => string

    createRoot((dispose) => {
      disposeRoot = dispose
      phaseAccessor = createConnectionPhase(conn, { graceMs: 15, offlineMs: 40, backMs: 30 })
    })
    cleanup.push(() => disposeRoot?.())

    conn.connect()
    await waitFor(() => conn.state() === "open")
    phases.push(phaseAccessor())

    // Simulate drop.
    srv.closeAll(1012)
    await waitFor(() => phaseAccessor() === "reconnecting")
    phases.push(phaseAccessor())
    await waitFor(() => phaseAccessor() === "offline")
    phases.push(phaseAccessor())

    // Reconnect manually.
    conn.connect()
    await waitFor(() => phaseAccessor() === "back")
    phases.push(phaseAccessor())
    await waitFor(() => phaseAccessor() === "ok")
    phases.push(phaseAccessor())

    expect(phases).toEqual(["ok", "reconnecting", "offline", "back", "ok"])
  })
})
