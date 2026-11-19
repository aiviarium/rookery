// The wire the client and the SDK speak, checked against the server's own report. `api/methods.json`
// is written by `cargo test -p rookery-server` from the methods `rookery serve` registers, so a method
// the client calls that left the server, and a method that arrived and is still called untyped, both
// fail here.
import { describe, expect, test } from "bun:test"
import { fileURLToPath } from "node:url"
import { type ClientRuntime, createClientApi } from "../src/client/runtime"
import type { JobEnd, LogItem, Manifest } from "../src/generated"
import { disposable } from "../src/types"
import { NOT_ON_SERVER, NOTIFICATIONS, WIRE } from "../src/wire"

const manifest = {
  id: "demo",
  name: "Demo",
  version: "0.1.0",
  contributes: { collections: [] },
} as unknown as Manifest

/** A plugin's `rookery` handle over a runtime that records what reaches the shell. */
function pluginRuntime(overrides: Partial<ClientRuntime> = {}) {
  const runtime: ClientRuntime = {
    call: async () => ({}),
    stream: async () => ({ ...aStream() }),
    onNotification: () => disposable(() => {}),
    onReconnect: () => disposable(() => {}),
    subscribe: async () => disposable(() => {}),
    runCommand: async () => ({}),
    registerView: () => disposable(() => {}),
    registerCommand: () => disposable(() => {}),
    ...overrides,
  }
  return createClientApi(runtime, "demo", manifest).rookery
}

function aStream() {
  return {
    id: 1,
    result: { stream: 1 },
    write: async () => {},
    eof: () => {},
    close: () => {},
    onData: (_: Uint8Array) => {},
    onEnd: () => {},
  }
}

// `sdk/test/` -> the repository root.
const root = fileURLToPath(new URL("../../", import.meta.url))
const report = (await Bun.file(`${root}/api/methods.json`).json()) as Array<{
  name: string
}>
const server = new Set(report.map((method) => method.name))

describe("the wire of @rookery/client and @rookery/sdk", () => {
  test("every method the client calls with generated types is one the server has", () => {
    const missing = Object.keys(WIRE).filter((method) => !server.has(method))
    expect(missing).toEqual([])
  })

  test("no method the client calls untyped has arrived on the server", () => {
    const arrived = NOT_ON_SERVER.filter((method) => server.has(method))
    expect(arrived).toEqual([])
  })

  test("no method is both typed and untyped", () => {
    const both = Object.keys(WIRE).filter((method) => (NOT_ON_SERVER as readonly string[]).includes(method))
    expect(both).toEqual([])
  })

  test("every method the client's code calls by name is known", async () => {
    // A call by a literal name: `conn.call("jobs.get", ...)`, `wireCall(conn, "auth.me", ...)`, and
    // the notifications it listens for with `conn.on("events.event", ...)`.
    const call = /(?:call|stream|notify|wireCall|onNotification|on)\(\s*(?:\w+\s*,\s*)?"([a-z][\w]*\.[\w]+)"/g
    const notifications: readonly string[] = NOTIFICATIONS
    const untyped = new Set<string>(NOT_ON_SERVER)
    const unknown: Array<{ file: string; method: string }> = []
    for (const dir of ["client/src", "sdk/src"]) {
      for (const path of new Bun.Glob(`${dir}/**/*.ts`).scanSync({ cwd: root })) {
        const text = await Bun.file(`${root}/${path}`).text()
        for (const match of text.matchAll(call)) {
          const method = match[1]!
          if (server.has(method) || notifications.includes(method) || untyped.has(method)) continue
          unknown.push({ file: path, method })
        }
      }
    }
    expect(unknown).toEqual([])
  })

  test("params reach the wire with the names the server takes", async () => {
    const calls: Array<{ method: string; params: Record<string, unknown> }> = []
    const notifications: string[] = []
    const streamed: Array<{ method: string; params: Record<string, unknown> }> = []
    const rookery = pluginRuntime({
      call: async (method, params) => {
        calls.push({ method, params })
        if (method === "settings.get") return { key: "autoSave", value: true, scope: "user" }
        if (method === "storage.get") return { key: "k", value: 1, revision: 2, updatedAt: null }
        if (method === "storage.set") return { revision: 3 }
        if (method === "storage.delete") return { deleted: true }
        if (method === "storage.list") return { entries: [] }
        if (method === "settings.list") return { entries: [] }
        if (method === "jobs.enqueue") return { id: "job-1" }
        if (method === "jobs.list") return { jobs: [] }
        if (method === "jobs.output") return { items: [], state: "succeeded" }
        if (method === "events.publish") return { delivered: 1, dropped: 0 }
        if (method === "events.subscribe") return { subscription: "sub-1" }
        if (method === "notify.list") return { notifications: [], unread: 0 }
        if (method === "notify.read") return { count: 2 }
        return {}
      },
      stream: async (method, params) => {
        streamed.push({ method, params })
        return {
          id: 7,
          result: { stream: 7, offset: 0 },
          write: async () => {},
          eof: () => {},
          close: () => {},
          onData: () => {},
          onEnd: () => {},
        }
      },
      onNotification: (method) => {
        notifications.push(method)
        return disposable(() => {})
      },
      subscribe: async (names) => {
        calls.push({
          method: "events.subscribe",
          params: { names: [...(typeof names === "string" ? [names] : names)] },
        })
        return disposable(() => {})
      },
    })

    await rookery.events.subscribe("hosts.changed", () => {})
    await rookery.events.subscribe(["demo/a", "fleet/*"], () => {})
    expect(calls.filter((c) => c.method === "events.subscribe").map((c) => c.params)).toEqual([
      { names: ["demo/hosts.changed"] },
      { names: ["demo/a", "fleet/*"] },
    ])

    await rookery.events.publish("demo/thing", { n: 1 })
    expect(calls.at(-1)).toEqual({
      method: "events.publish",
      params: { plugin: "demo", name: "thing", payload: { n: 1 } },
    })

    expect(await rookery.settings.get("autoSave")).toBe(true)
    expect(calls.at(-1)).toEqual({ method: "settings.get", params: { plugin: "demo", key: "autoSave" } })
    await rookery.settings.set("autoSave", false, "user")
    expect(calls.at(-1)).toEqual({
      method: "settings.set",
      params: { plugin: "demo", key: "autoSave", value: false, scope: "user" },
    })
    await rookery.settings.list()
    expect(calls.at(-1)).toEqual({ method: "settings.list", params: { plugin: "demo" } })

    expect(await rookery.storage.get("k")).toBe(1)
    expect(calls.at(-1)).toEqual({
      method: "storage.get",
      params: { plugin: "demo", key: "k", scope: "global" },
    })
    expect(await rookery.storage.set("k", 2, { revision: 2 })).toEqual({ revision: 3 })
    expect(calls.at(-1)).toEqual({
      method: "storage.set",
      params: { plugin: "demo", key: "k", value: 2, scope: "global", revision: 2 },
    })
    await rookery.storage.list({ prefix: "probe:", scope: "user" })
    expect(calls.at(-1)).toEqual({
      method: "storage.list",
      params: { plugin: "demo", prefix: "probe:", scope: "user" },
    })
    expect(await rookery.storage.delete("k")).toBe(true)
    expect(calls.at(-1)).toEqual({
      method: "storage.delete",
      params: { plugin: "demo", key: "k", scope: "global" },
    })

    await rookery.jobs.enqueue("build", { host: "studio" }, { subject: "env://studio" })
    expect(calls.at(-1)).toEqual({
      method: "jobs.enqueue",
      params: { kind: "build", params: { host: "studio" }, subject: "env://studio" },
    })
    await rookery.jobs.list({ state: "failed", limit: 10 })
    expect(calls.at(-1)).toEqual({
      method: "jobs.list",
      params: { state: "failed", limit: 10 },
    })
    await rookery.jobs.output("job-1")
    expect(calls.at(-1)).toEqual({ method: "jobs.output", params: { id: "job-1", after: 0 } })

    await rookery.jobs.follow("job-1", () => {})
    expect(streamed.at(-1)).toEqual({ method: "jobs.follow", params: { id: "job-1", after: 0 } })

    expect(await rookery.notify.list()).toEqual({ notifications: [], unread: 0 })
    expect(calls.at(-1)).toEqual({ method: "notify.list", params: { unread: false } })
    expect(await rookery.notify.read()).toBe(2)
    expect(calls.at(-1)).toEqual({ method: "notify.read", params: {} })

    rookery.notifications.on("events.dropped", () => {})
    expect(notifications).toEqual(["events.dropped"])
  })

  test("a followed job's log is LogItem lines and a last JobEnd line", async () => {
    let emit: (chunk: Uint8Array) => void = () => {}
    let end: () => void = () => {}
    const rookery = pluginRuntime({
      stream: async () => {
        const stream = { ...aStream(), id: 7, result: { stream: 7, offset: 0 } }
        emit = (chunk) => stream.onData(chunk)
        end = () => stream.onEnd()
        return stream
      },
    })
    const items: LogItem[] = []
    const ends: JobEnd[] = []
    await rookery.jobs.follow("job-1", (item) => items.push(item), { onDone: (e) => ends.push(e) })

    const enc = new TextEncoder()
    // One line arrives in two chunks, so the SDK has to join it before reading it.
    emit(enc.encode('{"seq":1,"at":"2026-01-01T00:00:00.000Z","attempt":0,"level":"info","text":"hel'))
    emit(
      enc.encode(
        'lo","data":null}\n{"seq":2,"at":"2026-01-01T00:00:01.000Z","attempt":1,"level":"warn","text":"world","data":{"n":2}}\n',
      ),
    )
    emit(enc.encode('{"state":"failed","result":null,"error":{"code":"INTERNAL","message":"boom"}}\n'))
    end()

    expect(items.map((i) => [i.seq, i.text, i.level])).toEqual([
      [1, "hello", "info"],
      [2, "world", "warn"],
    ])
    expect(ends).toEqual([{ state: "failed", result: null, error: { code: "INTERNAL", message: "boom" } }])
  })
})
