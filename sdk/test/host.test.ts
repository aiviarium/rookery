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
