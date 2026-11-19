import { openSync, readFileSync, writeSync } from "node:fs"
import { definePlugin, z } from "@rookery/sdk"

export default definePlugin((rookery) => {
  rookery.rpc.handle("greet", {
    level: "own",
    params: z.object({ name: z.string() }),
    run: (ctx, { name }) => ({ greeting: `Hello, ${name}`, as: ctx.principal.id, from: rookery.id }),
  })
  rookery.rpc.handle("reset", { level: "admin", run: () => ({ reset: true }) })
  rookery.rpc.handle("remember", {
    level: "own",
    params: z.object({ value: z.string() }),
    run: async (_, { value }) => {
      await rookery.storage.set("last", value)
      return { stored: await rookery.storage.get("last") }
    },
  })
  rookery.rpc.handle("setting", {
    level: "own",
    run: async () => ({ value: await rookery.settings.get("greeting" as never) }),
  })
  rookery.rpc.handle("env", {
    level: "own",
    run: () => ({
      pid: process.pid,
      keys: Object.keys(process.env)
        .filter((k) => k.startsWith("ROOKERY_"))
        .sort(),
    }),
  })
  // Writes straight to the pipe to the server, saying it is another plugin.
  rookery.rpc.handle("forge", {
    level: "own",
    params: z.object({ as: z.string() }),
    run: (_, { as }) => {
      const line = (m: unknown) => writeSync(1, `${JSON.stringify(m)}\n`)
      line({
        jsonrpc: "2.0",
        id: 990001,
        method: "storage.set",
        plugin: as,
        params: { key: "forged", value: true },
      })
      line({
        jsonrpc: "2.0",
        method: "plugins.unregistered",
        params: { plugin: as, kind: "rpc", key: "greet" },
      })
      return { forged: as }
    },
  })
  // Tries to open another process's pipes, and says what the kernel answered.
  rookery.rpc.handle("peek", {
    level: "own",
    params: z.object({ pid: z.number() }),
    run: (_, { pid }) => {
      const tried = (path: string, open: (p: string) => unknown) => {
        try {
          open(path)
          return "opened"
        } catch (e) {
          return (e as { code?: string }).code ?? String(e)
        }
      }
      return {
        fd: tried(`/proc/${pid}/fd/0`, (p) => openSync(p, "w")),
        environ: tried(`/proc/${pid}/environ`, (p) => readFileSync(p)),
      }
    },
  })
  // Holds memory until it is ended.
  rookery.rpc.handle("hog", {
    level: "own",
    params: z.object({ mb: z.number() }),
    run: async (ctx, { mb }) => {
      const held = Array.from({ length: mb }, () => Buffer.alloc(1 << 20, 1))
      await new Promise((resolve) => ctx.signal.addEventListener("abort", resolve))
      return held.length
    },
  })
  rookery.rpc.handle("slow", {
    level: "own",
    run: (ctx) =>
      new Promise((resolve) => {
        const timer = setTimeout(() => resolve("late"), 60_000)
        ctx.signal.addEventListener("abort", () => clearTimeout(timer))
      }),
  })
  // Ignores its deadline, and keeps the worker busy past it.
  rookery.rpc.handle("stuck", {
    level: "own",
    run: () => {
      const until = Date.now() + 30_000
      while (Date.now() < until) {}
      return "done"
    },
  })
  rookery.rpc.handle("crash", { level: "own", run: () => process.exit(3) })
  // Jobs its manifest may declare: echo.build, echo.wait and echo.wipe.
  rookery.jobs.handle("echo.build", {
    params: z.object({ n: z.number() }),
    run: async (job, { n }) => {
      await job.progress(1, 2, "halfway")
      await job.log(`building ${n}`)
      return { built: n * 2, attempt: job.attempt, kind: job.context.name }
    },
  })
  rookery.jobs.handle("echo.wait", {
    run: (job) =>
      new Promise((_, reject) => job.signal.addEventListener("abort", () => reject(job.signal.reason))),
  })
  rookery.jobs.handle("echo.wipe", { run: () => ({ wiped: true }) })
  // Events of its own, by a name without a `/`.
  const heard: string[] = []
  rookery.rpc.handle("listen", {
    level: "own",
    run: async () => {
      await rookery.events.subscribe("pinged", (_, name) => {
        heard.push(name)
      })
      return { listening: true }
    },
  })
  rookery.rpc.handle("ping", {
    level: "own",
    run: async () => {
      await rookery.events.publish("pinged", { at: 1 })
      return { pinged: true }
    },
  })
  rookery.rpc.handle("heard", { level: "own", run: () => ({ heard }) })
  rookery.onDispose(async () => {
    await rookery.storage.set("disposed", true)
  })
})
