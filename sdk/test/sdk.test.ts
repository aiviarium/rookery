import { describe, expect, test } from "bun:test"
import { defineClient } from "../src/client/index"
import { createClientApi, isClientPlugin } from "../src/client/runtime"
import { binaryRow, createCollection, mentions, plainText } from "../src/data"
import { isErrorCode, RookeryError } from "../src/errors"
import { compareRows, type FilterCollection, parseFilter } from "../src/filter"
import type { Manifest } from "../src/generated"
import { definePlugin, isHostPlugin } from "../src/index"
import { disposable } from "../src/types"

describe("@rookery/sdk errors & disposables", () => {
  test("RookeryError preserves code, message, details, and error code guard", () => {
    const err = new RookeryError("NOT_FOUND", "missing item", { id: "a1" })
    expect(err).toBeInstanceOf(Error)
    expect(err).toBeInstanceOf(RookeryError)
    expect(isErrorCode(err.code)).toBe(true)
    expect(isErrorCode("NOPE")).toBe(false)
    expect(err.code).toBe("NOT_FOUND")
    expect(err.message).toBe("missing item")
    expect(err.details).toEqual({ id: "a1" })
  })

  test("RookeryError.from wraps arbitrary values and preserves existing RookeryError", () => {
    const orig = new RookeryError("INVALID", "bad input")
    expect(RookeryError.from(orig)).toBe(orig)
    const wrapped = RookeryError.from({ code: "CONFLICT", message: "duplicate" })
    expect(wrapped.code).toBe("CONFLICT")
    expect(wrapped.message).toBe("duplicate")
    const fallback = RookeryError.from("boom")
    expect(fallback.code).toBe("INTERNAL")
    expect(fallback.message).toBe("boom")
  })

  test("disposable runs cleanup exactly once", () => {
    let calls = 0
    const d = disposable(() => {
      calls++
    })
    d.dispose()
    d.dispose()
    expect(calls).toBe(1)
  })
})

describe("@rookery/sdk filter & sort", () => {
  const collection: FilterCollection = {
    scope: "global",
    trash: false,
    fields: [
      { name: "status", type: "text", index: true },
      { name: "priority", type: "integer", index: true },
      { name: "title", type: "text" },
      { name: "done", type: "boolean" },
    ] as FilterCollection["fields"],
  }

  test("parseFilter matches equality, comparisons, and boolean combinators", () => {
    const filter = parseFilter(collection, {
      status: "open",
      or: [{ priority: { gte: 2 } }, { done: false }],
    })
    expect(filter.matches({ id: "1", status: "open", priority: 3, done: true })).toBe(true)
    expect(filter.matches({ id: "2", status: "open", priority: 1, done: false })).toBe(true)
    expect(filter.matches({ id: "3", status: "open", priority: 1, done: true })).toBe(false)
    expect(filter.matches({ id: "4", status: "closed", priority: 5, done: false })).toBe(false)
  })

  test("compareRows sorts ascending and descending with id tie-breaker", () => {
    const rows = [
      { id: "b", priority: 2 },
      { id: "a", priority: 2 },
      { id: "c", priority: 1 },
    ]
    rows.sort(compareRows(collection, [["priority", "asc"]]))
    expect(rows.map((r) => r.id)).toEqual(["c", "a", "b"])
    rows.sort(compareRows(collection, [["priority", "desc"]]))
    expect(rows.map((r) => r.id)).toEqual(["a", "b", "c"])
  })
})

describe("@rookery/sdk data helpers", () => {
  test("plainText and mentions extract text and mention URIs from RichText docs", () => {
    const doc = {
      type: "doc" as const,
      content: [
        {
          type: "paragraph",
          content: [
            { type: "text", text: "Hello " },
            { type: "mention", attrs: { uri: "user:u-alice", label: "Alice" } },
            { type: "text", text: "!" },
          ],
        },
      ],
    }
    expect(plainText(doc)).toBe("Hello @Alice!")
    expect(mentions(doc)).toEqual(["user:u-alice"])
  })

  test("binaryRow encodes Uint8Array fields to base64 and decodes them back", () => {
    const def = {
      fields: [{ name: "payload", type: "bytes" }],
    }
    const raw = new Uint8Array([1, 2, 3, 255])
    const encoded = binaryRow({ id: "r1", payload: raw }, def, true) as Record<string, unknown>
    expect(typeof encoded.payload).toBe("string")
    const decoded = binaryRow(encoded, def, false) as Record<string, unknown>
    expect(decoded.payload).toEqual(raw)
  })

  test("createCollection builds data.find RPC params cleanly", async () => {
    const calls: Array<{ method: string; params: Record<string, unknown> }> = []
    const coll = createCollection<{
      status?: string
      title?: string
      id: string
      createdAt: string
      updatedAt: string
      createdBy: string | null
      updatedBy: string | null
      rev: number
    }>(async (method, params) => {
      calls.push({ method, params })
      if (method === "data.find") return { rows: [{ id: "1", title: "Item" }], next: null }
      if (method === "data.get") return { row: { id: "1", title: "Item" } }
      return { id: "1", rev: 1 }
    }, "tasks")

    const page = await coll.find({ where: { status: "open" }, limit: 10 })
    expect(page.rows).toHaveLength(1)
    expect(calls[0]).toEqual({
      method: "data.find",
      params: { collection: "tasks", query: { where: { status: "open" }, limit: 10 } },
    })
  })
})

describe("@rookery/sdk definePlugin & defineClient", () => {
  test("definePlugin and defineClient brand definitions", () => {
    const hostPlugin = definePlugin(() => {})
    const clientPlugin = defineClient(() => {})
    expect(isHostPlugin(hostPlugin)).toBe(true)
    expect(isClientPlugin(clientPlugin)).toBe(true)
    expect(isHostPlugin(clientPlugin)).toBe(false)
    expect(isClientPlugin(hostPlugin)).toBe(false)
  })

  test("createClientApi tracks and disposes registrations in reverse order", async () => {
    const disposed: string[] = []
    const manifest = {
      id: "demo",
      name: "Demo",
      version: "0.1.0",
      contributes: {
        views: [{ id: "demo.home", title: "Demo" }],
        routes: [],
        commands: [],
        collections: [],
        settings: [],
        adminPages: [],
        nav: [],
      },
    } as unknown as Manifest

    const { rookery, dispose } = createClientApi(
      {
        call: async () => ({}),
        stream: async () => {
          throw new Error("unused")
        },
        subscribe: async () => disposable(() => disposed.push("event")),
        onNotification: () => disposable(() => {}),
        onReconnect: () => disposable(() => {}),
        runCommand: async () => ({}),
        registerView: () => disposable(() => disposed.push("view")),
        registerCommand: () => disposable(() => disposed.push("cmd")),
        registerStatusItem: () => disposable(() => disposed.push("status")),
        registerNavSection: () => disposable(() => disposed.push("nav")),
        registerChromeComponent: () => disposable(() => disposed.push("chrome")),
      },
      "demo",
      manifest,
    )
    rookery.views.register("demo.home", () => null)
    rookery.commands.register("demo.run", { title: "Run", run: () => {} })
    await rookery.events.subscribe("demo.tick", () => {})
    dispose()
    expect(disposed).toEqual(["event", "cmd", "view"])
  })

  test("client blocks, widgets, entity renderers, workbench.share, and env.process", async () => {
    const blocks = new Map<string, unknown>()
    const widgets = new Map<string, unknown>()
    const renderers = new Map<string, (props: any) => any>()
    const shared: string[] = []
    const opened: string[] = []
    const calls: Array<{ method: string; params: Record<string, unknown> }> = []
    const streamCallbacks: { onData?: (b: Uint8Array) => void; onEnd?: (e?: Error) => void } = {}
    const written: Uint8Array[] = []
    let eofCalled = false
    let closed = false

    const manifest = {
      id: "demo",
      name: "Demo",
      version: "0.1.0",
      contributes: {
        views: [],
        routes: [],
        commands: [],
        collections: [],
        settings: [],
        adminPages: [],
        nav: [],
        blocks: [{ id: "demo.card", title: "Card" }],
        widgets: [{ id: "demo.clock", title: "Clock", sizes: { default: { w: 4, h: 2 } } }],
        entityTypes: [{ scheme: "demo", title: "Demo" }],
      },
    } as unknown as Manifest

    const { rookery, dispose } = createClientApi(
      {
        call: async (method, params) => {
          calls.push({ method, params })
          return {}
        },
        stream: async (method, params) => {
          calls.push({ method, params })
          const s = {
            id: 7,
            result: { stream: 7, process: "proc-42" },
            write: (bytes: Uint8Array) => written.push(bytes),
            eof: () => {
              eofCalled = true
            },
            close: () => {
              closed = true
            },
            set onData(fn: (b: Uint8Array) => void) {
              streamCallbacks.onData = fn
            },
            set onEnd(fn: (e?: Error) => void) {
              streamCallbacks.onEnd = fn
            },
          }
          return s as any
        },
        subscribe: async () => disposable(() => {}),
        onNotification: () => disposable(() => {}),
        onReconnect: () => disposable(() => {}),
        runCommand: async () => ({}),
        registerView: () => disposable(() => {}),
        registerCommand: () => disposable(() => {}),
        registerBlock: (_p, id, component) => {
          blocks.set(id, component)
          return disposable(() => blocks.delete(id))
        },
        registerWidget: (_p, id, component) => {
          widgets.set(id, component)
          return disposable(() => widgets.delete(id))
        },
        widgetTypes: () => [
          { plugin: "demo", id: "demo.clock", title: "Clock", sizes: { default: { w: 4, h: 2 } } } as any,
        ],
        registerEntityRenderer: (_p, kind, scheme, component) => {
          const key = `${kind}:${scheme}`
          renderers.set(key, component)
          return disposable(() => renderers.delete(key))
        },
        entityRenderer: (kind, scheme) => renderers.get(`${kind}:${scheme}`),
        openEntity: (uri) => opened.push(uri),
        share: (uri) => shared.push(uri),
      },
      "demo",
      manifest,
    )

    // Blocks
    expect(() => rookery.blocks.register("demo.nope", () => null)).toThrow(
      "not declared in contributes.blocks",
    )
    rookery.blocks.register("demo.card", () => "block-el")
    expect(blocks.has("demo.card")).toBe(true)

    // Widgets
    rookery.widgets.register("demo.clock", () => "widget-el")
    expect(widgets.has("demo.clock")).toBe(true)
    expect(rookery.widgets.types().map((w) => w.id)).toEqual(["demo.clock"])

    // Entities
    expect(() => rookery.entities.card("other", () => null)).toThrow("not declared in plugin.json")
    expect(rookery.entities.renders("card", "demo")).toBe(false)
    rookery.entities.card("demo", (p) => `card:${p.scheme}:${p.id}:${p.density}`)
    rookery.entities.row("demo", (p) => `row:${p.scheme}:${p.id}`)
    expect(rookery.entities.renders("card", "demo")).toBe(true)
    expect(rookery.entities.renders("row", "demo")).toBe(true)
    expect(rookery.entities.Card({ uri: "demo:123" })).toBe("card:demo:123:compact")
    expect(rookery.entities.Row({ uri: "demo:123" })).toBe("row:demo:123")
    expect(rookery.entities.Card({ uri: "unknown:1", fallback: "fb" })).toBe("fb")
    rookery.entities.open("demo:123")
    expect(opened).toEqual(["demo:123"])

    // Workbench share
    rookery.workbench.share("doc:abc")
    expect(shared).toEqual(["doc:abc"])

    // env.process demux & signal
    const proc = await rookery.env.process("local", ["rg", "needle"], { cwd: "/repo" })
    expect(proc.id).toBe("proc-42")
    const out: string[] = []
    const err: string[] = []
    let exit: unknown
    const dec = new TextDecoder()
    proc.onStdout = (b) => out.push(dec.decode(b))
    proc.onStderr = (b) => err.push(dec.decode(b))
    proc.onExit = (e) => {
      exit = e
    }
    const enc = new TextEncoder()
    streamCallbacks.onData?.(new Uint8Array([1, ...enc.encode("match\n")]))
    streamCallbacks.onData?.(new Uint8Array([2, ...enc.encode("warn\n")]))
    streamCallbacks.onData?.(new Uint8Array([3, ...enc.encode(JSON.stringify({ exitCode: 0, lost: false }))]))
    expect(out).toEqual(["match\n"])
    expect(err).toEqual(["warn\n"])
    expect(exit).toEqual({ exitCode: 0, lost: false })

    proc.write("hi")
    proc.eof()
    await proc.signal("INT")
    proc.close()
    expect(written.length).toBe(1)
    expect(dec.decode(written[0])).toBe("hi")
    expect(eofCalled).toBe(true)
    expect(closed).toBe(true)
    expect(calls[1]).toEqual({
      method: "env.signal",
      params: { env: "local", process: "proc-42", signal: "INT" },
    })

    dispose()
    expect(blocks.size).toBe(0)
    expect(widgets.size).toBe(0)
    expect(renderers.size).toBe(0)
  })
})
