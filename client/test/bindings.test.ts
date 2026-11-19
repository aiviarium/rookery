import { afterEach, describe, expect, test } from "bun:test"
import type { FilterCollection } from "@rookery/sdk/filter"
import { createRoot } from "solid-js"
import {
  createDataClient,
  createEventSubscription,
  createLiveQuery,
  createRpcResource,
  createStream,
} from "../src/bindings"
import { createEventBus } from "../src/events"
import type { Manifest, PluginInfo } from "../src/generated"
import { buildRouteTable, matchUrl, viewUrl } from "../src/routes"
import { Connection } from "../src/ws"

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

describe("@rookery/client SolidJS bindings", () => {
  test("createLiveQuery loads initial rows, applies live changes, and unlives on cleanup", async () => {
    const sockets = new Set<any>()
    const unlived: string[] = []
    const liveSeq = 10

    const server = Bun.serve({
      port: 0,
      fetch(req, srv) {
        if (new URL(req.url).pathname === "/ws" && srv.upgrade(req)) return undefined
        return new Response("not found", { status: 404 })
      },
      websocket: {
        open(ws) {
          sockets.add(ws)
        },
        close(ws) {
          sockets.delete(ws)
        },
        message(ws, data) {
          const msg = JSON.parse(String(data))
          if (msg.method === "data.live") {
            ws.send(
              JSON.stringify({
                jsonrpc: "2.0",
                id: msg.id,
                result: {
                  subscription: "sub-1",
                  seq: liveSeq,
                  rows: [
                    { id: "r1", status: "open", order: 1, title: "First" },
                    { id: "r2", status: "open", order: 2, title: "Second" },
                  ],
                  next: null,
                },
              }),
            )
            return
          }
          if (msg.method === "data.unlive") {
            unlived.push(msg.params.subscription)
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }))
          }
        },
      },
    })
    cleanup.push(() => server.stop(true))

    const notify = (method: string, params: unknown) => {
      const payload = JSON.stringify({ jsonrpc: "2.0", method, params })
      for (const ws of sockets) ws.send(payload)
    }

    const conn = new Connection({
      url: `ws://127.0.0.1:${server.port}/ws`,
      checkSession: async () => true,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())
    conn.connect()

    const coll: FilterCollection = {
      scope: "global",
      trash: false,
      fields: [
        { name: "status", type: "text" },
        { name: "order", type: "integer" },
        { name: "title", type: "text" },
      ] as FilterCollection["fields"],
    }

    let live!: ReturnType<typeof createLiveQuery>
    let disposeRoot!: () => void
    createRoot((dispose) => {
      disposeRoot = dispose
      live = createLiveQuery(conn, { plugin: "demo", collection: "tasks" }, coll, {
        where: { status: "open" },
        orderBy: [["order", "asc"]],
      })
    })

    await waitFor(() => !live.loading)
    expect(live().map((r) => r.id)).toEqual(["r1", "r2"])

    // Live insert of a matching row + non-matching row.
    notify("data.changed", {
      subscription: "sub-1",
      changes: [
        {
          seq: 11,
          collection: "tasks",
          id: "r0",
          op: "insert",
          rev: 1,
          spaceId: null,
          at: "2026-01-01T00:00:00Z",
          row: { id: "r0", status: "open", order: 0, title: "Zero" },
        },
        {
          seq: 12,
          collection: "tasks",
          id: "r9",
          op: "insert",
          rev: 1,
          spaceId: null,
          at: "2026-01-01T00:00:01Z",
          row: { id: "r9", status: "closed", order: 9, title: "Closed" },
        },
      ],
    })
    await waitFor(() => live().length === 3)
    expect(live().map((r) => r.id)).toEqual(["r0", "r1", "r2"])

    // Update row to no longer match filter -> removed.
    notify("data.changed", {
      subscription: "sub-1",
      changes: [
        {
          seq: 13,
          collection: "tasks",
          id: "r1",
          op: "update",
          rev: 2,
          spaceId: null,
          at: "2026-01-01T00:00:02Z",
          row: { id: "r1", status: "closed", order: 1, title: "Done" },
        },
      ],
    })
    await waitFor(() => live().length === 2)
    expect(live().map((r) => r.id)).toEqual(["r0", "r2"])

    // Delete row.
    notify("data.changed", {
      subscription: "sub-1",
      changes: [
        {
          seq: 14,
          collection: "tasks",
          id: "r0",
          op: "delete",
          rev: 2,
          spaceId: null,
          at: "2026-01-01T00:00:03Z",
          row: null,
        },
      ],
    })
    await waitFor(() => live().length === 1)
    expect(live().map((r) => r.id)).toEqual(["r2"])

    disposeRoot()
    await waitFor(() => unlived.includes("sub-1"))
  })

  test("createDataClient, createStream, createEventSubscription, and createRpcResource", async () => {
    const sockets = new Set<any>()
    const server = Bun.serve({
      port: 0,
      fetch(req, srv) {
        if (new URL(req.url).pathname === "/ws" && srv.upgrade(req)) return undefined
        return new Response("not found", { status: 404 })
      },
      websocket: {
        open(ws) {
          sockets.add(ws)
        },
        close(ws) {
          sockets.delete(ws)
        },
        message(ws, data) {
          if (typeof data !== "string") return
          const msg = JSON.parse(data)
          if (msg.method === "data.get") {
            ws.send(
              JSON.stringify({
                jsonrpc: "2.0",
                id: msg.id,
                result: { row: { id: msg.params.id, name: "Item" } },
              }),
            )
          } else if (msg.method === "events.subscribe") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { subscription: "ev-1" } }))
          } else if (msg.method === "events.unsubscribe") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }))
          } else if (msg.method === "demo.status") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { healthy: true } }))
          } else if (msg.method === "demo.logStream") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { stream: 42 } }))
            const payload = new TextEncoder().encode("log-line")
            const frame = new Uint8Array(4 + payload.byteLength)
            new DataView(frame.buffer).setUint32(0, 42, false)
            frame.set(payload, 4)
            ws.send(frame)
            ws.send(JSON.stringify({ jsonrpc: "2.0", method: "streams.eof", params: { stream: 42 } }))
          }
        },
      },
    })
    cleanup.push(() => server.stop(true))

    const conn = new Connection({
      url: `ws://127.0.0.1:${server.port}/ws`,
      checkSession: async () => true,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())
    const bus = createEventBus(conn)
    cleanup.push(() => bus.dispose())
    conn.connect()

    const manifest = {
      contributes: {
        collections: [
          { name: "items", scope: "global", trash: false, fields: [{ name: "name", type: "text" }] },
        ],
      },
    } as unknown as Pick<Manifest, "contributes">

    const dc = createDataClient(conn, "demo", manifest)
    const row = await dc.collection("items").get("item-1")
    expect(row).toMatchObject({ id: "item-1", name: "Item" })

    const chunks: string[] = []
    const events: unknown[] = []
    let streamBinding!: ReturnType<typeof createStream>
    let rpcRes!: ReturnType<typeof createRpcResource<{ healthy: boolean }>>
    let disposeRoot!: () => void

    createRoot((dispose) => {
      disposeRoot = dispose
      streamBinding = createStream(
        conn,
        "demo.logStream",
        {},
        {
          onData: (b) => chunks.push(new TextDecoder().decode(b)),
        },
      )
      createEventSubscription(bus, "demo.ping", (p) => events.push(p))
      rpcRes = createRpcResource<{ healthy: boolean }>(conn, "demo.status")
    })
    cleanup.push(() => disposeRoot())

    await waitFor(() => chunks.join("") === "log-line" && streamBinding.state() === "closed")
    await waitFor(() => rpcRes()?.healthy === true)
    await Bun.sleep(20)

    for (const ws of sockets) {
      ws.send(
        JSON.stringify({
          jsonrpc: "2.0",
          method: "events.event",
          params: { subscription: "ev-1", topic: "demo.ping", payload: 7 },
        }),
      )
    }
    await waitFor(() => events.length === 1)
    expect(events).toEqual([7])
  })
})

describe("@rookery/client route matching", () => {
  test("buildRouteTable, matchUrl, and viewUrl handle static, param, wildcard, and query params", () => {
    const plugin = {
      id: "fleet",
      state: "active",
      manifest: {
        id: "fleet",
        namespace: "fleet",
        contributes: {
          routes: [
            { path: "", view: "fleet.home" },
            { path: "hosts/:name", view: "fleet.host" },
            { path: "files/*rel", view: "fleet.files" },
          ],
        },
      },
    } as unknown as PluginInfo

    const table = buildRouteTable([plugin], [{ view: "shell.home", pattern: "/" }])
    expect(matchUrl(table, "/")).toEqual({ view: "shell.home", plugin: "shell", path: "", params: {} })
    expect(matchUrl(table, "/fleet")).toEqual({ view: "fleet.home", plugin: "fleet", path: "", params: {} })
    expect(matchUrl(table, "/fleet/hosts/box-1", "?tab=metrics")).toEqual({
      view: "fleet.host",
      plugin: "fleet",
      path: "box-1",
      params: { name: "box-1", tab: "metrics" },
    })
    expect(matchUrl(table, "/fleet/files/a/b/c.txt")).toEqual({
      view: "fleet.files",
      plugin: "fleet",
      path: "a/b/c.txt",
      params: { rel: "a/b/c.txt" },
    })

    expect(viewUrl(table, "fleet.host", { params: { name: "box-2", tab: "logs" } })).toBe(
      "/fleet/hosts/box-2?tab=logs",
    )
    expect(viewUrl(table, "fleet.host", { path: "box-3" })).toBe("/fleet/hosts/box-3")
    expect(viewUrl(table, "fleet.files", { path: "dir/file.txt" })).toBe("/fleet/files/dir/file.txt")
  })
})
