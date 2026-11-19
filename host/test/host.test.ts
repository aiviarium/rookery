import { describe, expect, test } from "bun:test"
import { fileURLToPath } from "node:url"
import { RookeryError } from "@rookery/sdk/errors"
import { Host, type ServerLink } from "../src/host"

const fixture = (name: string) => fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url))

function server() {
  const calls: Array<{ method: string; params: Record<string, unknown>; plugin?: string }> = []
  const notes: Array<{ method: string; params: any }> = []
  const store = new Map<string, unknown>()
  const link: ServerLink = {
    async request(method, params, plugin) {
      calls.push({ method, params, plugin })
      if (method === "storage.set") {
        store.set(params.key as string, params.value)
        return { revision: 1 }
      }
      if (method === "storage.get")
        return { key: params.key, value: store.get(params.key as string) ?? null, revision: 1 }
      if (method === "settings.get") throw new RookeryError("FORBIDDEN", "not this one")
      return {}
    },
    notify(method, params) {
      notes.push({ method, params })
    },
  }
  return { link, calls, notes }
}

const manifest = (id: string) => ({
  id,
  name: id,
  version: "1.0.0",
  host: "host.ts",
  provides: { services: ["greeter"] },
  contributes: { jobs: ["echo.build", "echo.wait", "echo.wipe"].map((id) => ({ id })) },
})
const ctx = (deadlineMs = 5000) => ({
  callId: crypto.randomUUID(),
  user: null,
  principal: { kind: "user", id: "u1" },
  deadline: Date.now() + deadlineMs,
})

async function until(check: () => boolean) {
  for (let i = 0; i < 200 && !check(); i++) await Bun.sleep(10)
  expect(check()).toBe(true)
}

describe("the extension host", () => {
  test("activates a host half and runs what it registered, its calls going to the server as it", async () => {
    const { link, calls } = server()
    const host = new Host(link)
    const activated: any = await host.handle("plugins.activate", {
      plugin: "echo",
      dir: fixture("echo"),
      manifest: manifest("echo"),
    })
    expect(activated.registrations.map((r: any) => `${r.kind}:${r.key}`).sort()).toEqual([
      "job:echo.build",
      "job:echo.wait",
      "job:echo.wipe",
      "rpc:ask",
      "rpc:contributions",
      "rpc:crash",
      "rpc:env",
      "rpc:forge",
      "rpc:greet",
      "rpc:heard",
      "rpc:hog",
      "rpc:listen",
      "rpc:move",
      "rpc:peek",
      "rpc:ping",
      "rpc:remember",
      "rpc:reset",
      "rpc:setting",
      "rpc:slow",
      "rpc:stuck",
      "service:greeter",
    ])
    expect(activated.registrations.find((r: any) => r.key === "greet").params.properties.name.type).toBe(
      "string",
    )
    const greet = await host.handle("plugins.invoke", {
      plugin: "echo",
      kind: "rpc",
      key: "greet",
      params: { name: "Ada" },
      ctx: ctx(),
    })
    expect(greet).toEqual({ greeting: "Hello, Ada", as: "u1", from: "echo" })
    expect(
      await host.handle("plugins.invoke", {
        plugin: "echo",
        kind: "rpc",
        key: "remember",
        params: { value: "x" },
        ctx: ctx(),
      }),
    ).toEqual({ stored: "x" })
    expect(calls.filter((c) => c.method.startsWith("storage.")).map((c) => c.plugin)).toEqual([
      "echo",
      "echo",
    ])
    // The server's error comes back with its code.
    const refused = await host
      .handle("plugins.invoke", { plugin: "echo", kind: "rpc", key: "setting", params: {}, ctx: ctx() })
      .catch((e) => e)
    expect(RookeryError.from(refused).code).toBe("FORBIDDEN")
    // Bad params are INVALID, an unknown method NOT_FOUND.
    const bad = await host
      .handle("plugins.invoke", { plugin: "echo", kind: "rpc", key: "greet", params: {}, ctx: ctx() })
      .catch((e) => e)
    expect(RookeryError.from(bad).code).toBe("INVALID")
    const none = await host
      .handle("plugins.invoke", { plugin: "echo", kind: "rpc", key: "none", params: {}, ctx: ctx() })
      .catch((e) => e)
    expect(RookeryError.from(none).code).toBe("NOT_FOUND")
    await host.handle("plugins.deactivate", { plugin: "echo" })
    expect(host.plugins()).toEqual([])
  })

  test("activates a typed service and a job handler while using connections", async () => {
    const chunks: unknown[] = []
    const jobCalls: string[] = []
    const host = new Host({
      async request(method, params, plugin) {
        expect(plugin).toBe("sample")
        switch (method) {
          case "connections.list":
            return { connections: [] }
          case "connections.get":
            expect(params).toEqual({ id: "fixture" })
            return { name: "Fixture" }
          case "connections.resolve":
            expect(params).toEqual({ id: "fixture" })
            return { credential: "fixture-key" }
          case "host.stream.item":
            chunks.push(params.item)
            return null
          case "host.job.progress":
          case "host.job.log":
            jobCalls.push(method)
            return null
          default:
            throw new Error(`unexpected host call ${method}`)
        }
      },
      notify() {},
    })
    const plugin = "sample"
    try {
      const activated = await host.handle("plugins.activate", {
        plugin,
        dir: fixture("host-apis"),
        manifest: {
          ...manifest(plugin),
          interface: "interface.ts",
          provides: { services: ["sample"] },
          contributes: { jobs: [{ id: "sample.run" }] },
        },
      })
      expect(activated).toMatchObject({
        registrations: expect.arrayContaining([
          { kind: "job", key: "sample.run" },
          {
            kind: "service",
            key: "sample",
            methods: {
              describe: { stream: false },
              count: { stream: true },
              wait: { stream: false, timeoutSecs: 1 },
            },
          },
        ]),
      })
      const service = { plugin, kind: "service", key: "sample" }
      expect(
        await host.handle("plugins.invoke", {
          ...service,
          params: { method: "describe", params: { id: "fixture" } },
          ctx: ctx(),
        }),
      ).toBe("Fixture")
      await host.handle("plugins.invoke", {
        ...service,
        params: { method: "count", params: { to: 3 } },
        ctx: ctx(),
        stream: true,
      })
      expect(chunks).toEqual([1, 2, 3])
      expect(
        await host.handle("plugins.invoke", {
          plugin,
          kind: "job",
          key: "sample.run",
          params: { params: {}, job: { id: "fixture", attempt: 2, kind: "sample/run", maxAttempts: 3 } },
          ctx: ctx(),
        }),
      ).toEqual({ attempt: 2 })
      expect(jobCalls).toEqual(["host.job.log", "host.job.progress"])
    } finally {
      await host.stop(plugin, true)
    }
  })

  test("a call past its deadline, or cancelled, ends", async () => {
    const { link } = server()
    const host = new Host(link)
    await host.handle("plugins.activate", {
      plugin: "echo",
      dir: fixture("echo"),
      manifest: manifest("echo"),
    })
    const late = await host
      .handle("plugins.invoke", { plugin: "echo", kind: "rpc", key: "slow", params: {}, ctx: ctx(50) })
      .catch((e) => e)
    expect(RookeryError.from(late).code).toBe("TIMEOUT")
    const c = ctx()
    const running = host
      .handle("plugins.invoke", { plugin: "echo", kind: "rpc", key: "slow", params: {}, ctx: c })
      .catch((e) => e)
    await Bun.sleep(20)
    host.notification("plugins.cancel", { plugin: "echo", callId: c.callId })
    expect(RookeryError.from(await running).code).toBe("TIMEOUT")
    await host.handle("plugins.deactivate", { plugin: "echo" })
  })

  test("a worker that ends is told to the server, and a module that is no plugin is refused", async () => {
    const { link, notes } = server()
    const host = new Host(link)
    await host.handle("plugins.activate", {
      plugin: "crash",
      dir: fixture("crash"),
      manifest: manifest("crash"),
    })
    await until(() => notes.some((n) => n.method === "plugins.exited"))
    expect(notes.find((n) => n.method === "plugins.exited")?.params.plugin).toBe("crash")
    expect(host.plugins()).toEqual([])
    const refused = await host
      .handle("plugins.activate", { plugin: "broken", dir: fixture("broken"), manifest: manifest("broken") })
      .catch((e) => e)
    expect(RookeryError.from(refused).message).toContain("definePlugin")
    await host.handle("plugins.deactivate", { plugin: "broken" })
  })

  test("a transform step runs with a tx of its migration, as the plugin", async () => {
    const calls: Array<{ method: string; params: any; plugin?: string }> = []
    const notes: string[] = []
    const host = new Host({
      async request(method, params, plugin) {
        calls.push({ method, params, plugin })
        if (method === "data.txRows")
          return {
            rows: params.after
              ? []
              : [
                  { id: "n1", text: "one" },
                  { id: "n2", text: "two" },
                ],
          }
        if (method === "data.txInsert") return { row: { id: "c", ...(params.values as object) } }
        return {}
      },
      notify(method, params) {
        if (method === "plugins.log") notes.push(String(params.message))
      },
    })
    const t = { plugin: "migrating", dir: fixture("migrating"), migration: "m-1" }
    await host.handle("plugins.transform", { ...t, name: "copy" })
    expect(calls.map((c) => [c.method, c.plugin, c.params.migration])).toEqual([
      ["data.txRows", "migrating", "m-1"],
      ["data.txInsert", "migrating", "m-1"],
      ["data.txInsert", "migrating", "m-1"],
      ["data.txRows", "migrating", "m-1"],
    ])
    expect(calls[1]!.params.values).toEqual({ of: "n1", text: "ONE" })
    expect(notes).toEqual(["[copy] copied"])
    const failed = await host.handle("plugins.transform", { ...t, name: "broken" }).catch((e) => e)
    expect(RookeryError.from(failed).message).toContain("this step fails")
  })
})
