import { describe, expect, test } from "bun:test"
import { RookeryError } from "../src/errors"
import type { Manifest } from "../src/generated"
import { activate, type CallContext, createHost, type HostTransport } from "../src/host/runtime"
import { definePlugin } from "../src/index"

const manifest = {
  apiVersion: "1",
  id: "files",
  name: "Files",
  version: "1.0.0",
  provides: { services: ["files"] },
  contributes: { jobs: [{ id: "files.check" }] },
} as unknown as Manifest

/** A transport that records what the host half asks the server for. */
function recorder(answer: (method: string, params: Record<string, any>) => unknown) {
  const calls: Array<{ method: string; params: Record<string, any> }> = []
  const transport: HostTransport = {
    call: async (method, params) => {
      calls.push({ method, params })
      return answer(method, params)
    },
    registered: () => {},
    unregistered: () => {},
    log: () => {},
  }
  return { transport, calls }
}

const ctx: CallContext = {
  callId: "call-1",
  user: { id: "01a1f29a-9205-74ff-9584-61b930804b33", name: "Ada", admin: false },
  principal: { kind: "user", id: "01a1f29a-9205-74ff-9584-61b930804b33" },
  space: "01a1f29a-9205-74ff-9584-61b930804b99",
  deadline: Date.now() + 5_000,
}

/** A plugin whose service reads and writes files on a host. */
function filesPlugin() {
  return definePlugin((rookery) => {
    rookery.services.provide("files", {
      read: (context, { path }) => rookery.env.readFile(context, "studio", path),
      write: (context, { path, content }) => rookery.env.writeFile(context, "studio", path, content),
      who: async (context) => ({ callId: context.callId, user: context.user?.name ?? null }),
      exec: (context, { command, opts }) => rookery.env.exec(context, "studio", command, opts),
      list: (context) => rookery.env.checkouts(context, "studio"),
      clone: (context, { repository }) => rookery.env.clone(context, "studio", repository),
      tree: (context, { checkout, branch }) => rookery.env.worktree(context, "studio", checkout, branch),
      remove: (context, { path }) => rookery.env.removeCheckout(context, "studio", path),
    })
  })
}

async function run(transport: HostTransport, method: string, params: Record<string, unknown>) {
  const { runtime, rookery } = createHost("files", manifest, transport)
  await activate(runtime, rookery, filesPlugin())
  return runtime.invoke({
    plugin: "files",
    kind: "service",
    key: "files",
    params: { method, params },
    ctx,
  })
}

describe("@rookery/sdk host env", () => {
  test("a service's env call names its invocation, so the server knows whose it is", async () => {
    const { transport, calls } = recorder(() => ({
      size: 5,
      sha256: "0".repeat(64),
      base64: btoa("hello"),
    }))
    const result = await run(transport, "read", { path: "/srv/work/README.md" })
    expect(result).toMatchObject({ size: 5 })
    expect(calls).toEqual([
      {
        method: "host.env",
        params: {
          callId: "call-1",
          method: "env.readFile",
          params: { env: "studio", path: "/srv/work/README.md" },
        },
      },
    ])
  })

  test("the handler's ctx carries the invocation and its person", async () => {
    const { transport } = recorder(() => ({}))
    expect(await run(transport, "who", {})).toEqual({ callId: "call-1", user: "Ada" })
  })

  test("a write carries its bytes in base64 and its options", async () => {
    const { transport, calls } = recorder(() => ({ size: 2, sha256: "f".repeat(64) }))
    await run(transport, "write", { path: "/srv/work/a.txt", content: "hi" })
    expect(calls[0]?.params).toEqual({
      callId: "call-1",
      method: "env.writeFile",
      params: { env: "studio", path: "/srv/work/a.txt", base64: btoa("hi") },
    })
    const bytes = recorder(() => ({ size: 1, sha256: "e".repeat(64) }))
    await run(bytes.transport, "write", { path: "/srv/work/b.bin", content: new Uint8Array([7]) })
    expect(bytes.calls[0]?.params.params.base64).toBe(btoa("\u0007"))
  })

  test("the server's refusal reaches the handler as a RookeryError", async () => {
    const { transport } = recorder(() => {
      throw new RookeryError("FORBIDDEN", "this needs use on env://studio")
    })
    const failure = (await run(transport, "read", { path: "/srv/work/README.md" }).catch(
      (e) => e,
    )) as RookeryError
    expect(failure).toBeInstanceOf(RookeryError)
    expect(failure.code).toBe("FORBIDDEN")
  })

  test("a command carries its folder, variables, input and time limit", async () => {
    const { transport, calls } = recorder(() => ({ exitCode: 0, lost: false }))
    await run(transport, "exec", {
      command: ["git", "status", "--porcelain"],
      opts: { cwd: "/srv/work", env: { GIT_TERMINAL_PROMPT: "0" }, stdin: "x", timeoutMs: 2_500 },
    })
    expect(calls[0]?.params).toEqual({
      callId: "call-1",
      method: "env.exec",
      params: {
        env: "studio",
        command: ["git", "status", "--porcelain"],
        cwd: "/srv/work",
        vars: { GIT_TERMINAL_PROMPT: "0" },
        stdin: "x",
        timeoutMs: 2_500,
      },
    })
  })

  test("checkouts, clone, worktree and removeCheckout go as the handler's own", async () => {
    const { transport, calls } = recorder((_method, params) => {
      if (params.method === "env.checkouts") return { checkouts: [{ path: "/srv/work/src" }] }
      if (params.method === "env.removeCheckout") return null
      return { path: "/srv/work/src", env: "env://studio" }
    })
    expect(await run(transport, "list", {})).toEqual([{ path: "/srv/work/src" }])
    await run(transport, "clone", { repository: "https://example.test/acme/widgets.git" })
    await run(transport, "tree", { checkout: "/srv/work/widgets", branch: "feature/x" })
    await run(transport, "remove", { path: "/srv/work/widgets" })
    expect(calls.map((call) => call.params.method)).toEqual([
      "env.checkouts",
      "env.clone",
      "env.worktree",
      "env.removeCheckout",
    ])
    expect(calls[1]?.params).toEqual({
      callId: "call-1",
      method: "env.clone",
      params: { env: "studio", repository: "https://example.test/acme/widgets.git" },
    })
    expect(calls[2]?.params.params).toEqual({
      env: "studio",
      checkout: "/srv/work/widgets",
      branch: "feature/x",
    })
    expect(calls[3]?.params.params).toEqual({ env: "studio", path: "/srv/work/widgets" })
  })

  test("a job's env call names the job's invocation, and its person is none", async () => {
    const { transport, calls } = recorder((method) =>
      method === "host.env" ? { size: 4, sha256: "a".repeat(64), base64: btoa("one\n") } : {},
    )
    const { runtime, rookery } = createHost("files", manifest, transport)
    await activate(
      runtime,
      rookery,
      definePlugin((handle) => {
        handle.jobs.handle("files.check", {
          async run(job) {
            const data = await handle.env.readFile(job.ctx, "studio", "/srv/work/one.txt")
            await job.log(`read ${data.size}`)
            return { size: data.size, user: job.ctx.user }
          },
        })
      }),
    )
    const result = await runtime.invoke({
      plugin: "files",
      kind: "job",
      key: "files.check",
      params: {
        params: { path: "/srv/work/one.txt" },
        job: { id: "job-1", attempt: 1, maxAttempts: 1, kind: "files/check" },
      },
      ctx: { ...ctx, user: null, principal: { kind: "system", id: "job:job-1" } },
    })
    expect(result).toEqual({ size: 4, user: null })
    expect(calls.map((call) => call.method)).toEqual(["host.env", "host.job.log"])
    expect(calls[0]?.params).toEqual({
      callId: "call-1",
      method: "env.readFile",
      params: { env: "studio", path: "/srv/work/one.txt" },
    })
    expect(calls[1]?.params).toEqual({ callId: "call-1", level: "info", text: "read 4" })
  })
})

/** A plugin whose service reads and writes its collections for the person it serves. */
function notesPlugin() {
  return definePlugin((rookery) => {
    rookery.services.provide("notes", {
      find: (context, { query }) => context.data.notes.find(query),
      insert: (context, { values }) => context.data.notes.insert(values),
      elsewhere: (context, { space, query }) => context.data.space(space).notes.find(query),
      level: (context, { resource }) => context.access.level(resource),
      may: (context, { resource, level }) => context.access.check(resource, level),
      who: (context) => context.space,
      own: () => rookery.data.notes.find({ limit: 1 }),
      watch: async (context, { collection }) => {
        const seen: unknown[] = []
        const off = await context.data.changes.subscribe(collection, (change) => seen.push(change))
        // Hold the call open for the test to deliver a change through it.
        for (let i = 0; i < 500 && seen.length === 0; i++) await Bun.sleep(1)
        return { seen, off }
      },
      blob: (context, { id }) => rookery.blobs.get(context, id),
      schedule: (context) => rookery.schedules.list(context),
      log: (context) => rookery.audit.list(context, { limit: 5 }),
      record: () => rookery.audit.record({ action: "mapping.set" }),
    })
  })
}

const notesManifest = {
  apiVersion: "1",
  id: "notes",
  name: "Notes",
  version: "1.0.0",
  provides: { services: ["notes"] },
  contributes: {
    collections: [{ name: "notes", fields: [{ name: "title", type: "text" }] }],
  },
} as unknown as Manifest

async function notes(transport: HostTransport, method: string, params: Record<string, unknown>) {
  const { runtime, rookery } = createHost("notes", notesManifest, transport)
  await activate(runtime, rookery, notesPlugin())
  return runtime.invoke({
    plugin: "notes",
    kind: "service",
    key: "notes",
    params: { method, params },
    ctx,
  })
}

describe("@rookery/sdk host data", () => {
  test("a handler's data call names its invocation, so the server runs it as its person", async () => {
    const { transport, calls } = recorder(() => ({ rows: [], next: null }))
    await notes(transport, "find", { query: { where: { title: "One" } } })
    expect(calls).toEqual([
      {
        method: "host.data",
        params: {
          callId: "call-1",
          method: "data.find",
          params: {
            plugin: "notes",
            collection: "notes",
            space: "01a1f29a-9205-74ff-9584-61b930804b99",
            query: { where: { title: "One" } },
          },
        },
      },
    ])
  })

  test("a write carries its values, and the invocation's space, through the same pipe", async () => {
    const { transport, calls } = recorder(() => ({ row: { id: "note-1", title: "One" } }))
    expect(await notes(transport, "insert", { values: { title: "One" } })).toMatchObject({
      id: "note-1",
    })
    expect(calls[0]?.params).toMatchObject({
      method: "data.insert",
      params: { plugin: "notes", collection: "notes", values: { title: "One" } },
    })
  })

  test("a space of its own takes the place of the invocation's", async () => {
    const { transport, calls } = recorder(() => ({ rows: [], next: null }))
    await notes(transport, "elsewhere", { space: "01a1f29a-9205-74ff-9584-61b930804baa", query: {} })
    expect(calls[0]?.params.params.space).toBe("01a1f29a-9205-74ff-9584-61b930804baa")
  })

  test("the plugin's own data handle stays the plugin's, with no space of a person's", async () => {
    const { transport, calls } = recorder(() => ({ rows: [], next: null }))
    await notes(transport, "own", {})
    expect(calls[0]).toEqual({
      method: "data.find",
      params: { plugin: "notes", collection: "notes", query: { limit: 1 } },
    })
  })

  test("the context carries the invocation's space", async () => {
    const { transport } = recorder(() => ({}))
    expect(await notes(transport, "who", {})).toBe(ctx.space)
  })

  test("a level is checked for the person, by one call", async () => {
    const { transport, calls } = recorder(() => ({ level: "use" }))
    expect(await notes(transport, "level", { resource: "space://x" })).toBe("use")
    expect(await notes(transport, "may", { resource: "space://x", level: "use" })).toBe(true)
    expect(calls.map((c) => c.params.method)).toEqual(["access.level", "access.level"])
  })

  test("a change feed is a live query for the person, and ends with the call", async () => {
    const { transport, calls } = recorder(() => ({ subscription: "sub-1" }))
    const { runtime, rookery } = createHost("notes", notesManifest, transport)
    await activate(runtime, rookery, notesPlugin())
    const running = runtime.invoke({
      plugin: "notes",
      kind: "service",
      key: "notes",
      params: { method: "watch", params: { collection: "notes" } },
      ctx,
    })
    await Bun.sleep(10)
    expect(calls[0]?.params).toEqual({
      callId: "call-1",
      method: "data.live",
      params: {
        plugin: "notes",
        collection: "notes",
        space: ctx.space ?? undefined,
        query: {},
      },
    })
    runtime.notified("data.changed", {
      subscription: "sub-1",
      changes: [{ seq: 1, collection: "notes", id: "n1", op: "insert", rev: 1, row: null }],
    })
    const watched = (await running) as { seen: unknown[] }
    expect(watched.seen).toHaveLength(1)
    // The handler's call is over: its feed is not left running.
    expect(calls[1]?.params).toEqual({
      callId: "call-1",
      method: "data.unlive",
      params: { plugin: "notes", space: ctx.space, subscription: "sub-1" },
    })
  })

  test("blobs, schedules and the audit log run for the person; audit.record stays the plugin's", async () => {
    const { transport, calls } = recorder((_method, params) => {
      if (params.method === "blobs.get") return { id: "b1" }
      if (params.method === "schedules.list") return { schedules: [] }
      if (params.method === "audit.list") return { entries: [] }
      return {}
    })
    expect(await notes(transport, "blob", { id: "b1" })).toEqual({ id: "b1" })
    expect(await notes(transport, "schedule", {})).toEqual([])
    expect(await notes(transport, "log", {})).toEqual([])
    await notes(transport, "record", {})
    expect(calls.map((call) => call.method)).toEqual(["host.data", "host.data", "host.data", "audit.record"])
    expect(calls[0]?.params).toMatchObject({ callId: "call-1", method: "blobs.get" })
    expect(calls[1]?.params).toMatchObject({ callId: "call-1", method: "schedules.list" })
    expect(calls[2]?.params).toMatchObject({ callId: "call-1", method: "audit.list" })
    expect(calls[3]?.params).toEqual({ action: "mapping.set" })
  })
})
