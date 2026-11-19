import { parentPort } from "node:worker_threads"
import { definePlugin, RookeryError, z } from "@rookery/sdk"
import api from "./interface"

// The tests use the worker port directly to exercise the server's checks without the SDK.
let jobCallId: string | undefined
let next = -1
parentPort?.on("message", (message) => {
  if (message.type === "invoke" && message.invoke.kind === "job") {
    jobCallId = message.invoke.ctx.callId
  }
})

function hostCall(method: string, params: Record<string, unknown>): Promise<any> {
  const id = next--
  return new Promise((resolve, reject) => {
    const result = (message: any) => {
      if (message.type !== "result" || message.id !== id) return
      parentPort?.off("message", result)
      if (message.error)
        reject(new RookeryError(message.error.code, message.error.message, message.error.details))
      else resolve(message.result)
    }
    parentPort?.on("message", result)
    parentPort?.postMessage({ type: "call", id, method, params })
  })
}

export default definePlugin(async (rookery) => {
  await rookery.connections.list()
  if (rookery.id === "sample") {
    rookery.services.provide(api.services.sample, {
      describe: async (_ctx, { id }) => {
        const { name } = await rookery.connections.get(id)
        const { credential } = await rookery.connections.resolve(id)
        return credential ? name : "missing credential"
      },
      async *count(_ctx, { to }) {
        for (let i = 1; i <= to; i++) yield i
      },
      invalidResult: () => 123 as unknown as string,
      async *invalidChunk() {
        yield 123 as unknown as string
      },
      async *idle(ctx) {
        // An unread second chunk reaches the pipe while the first still occupies the buffer.
        for (let i = 0; !ctx.signal.aborted; i++) yield i
      },
      async *pending(ctx) {
        await new Promise<void>((resolve) =>
          ctx.signal.addEventListener("abort", () => resolve(), { once: true }),
        )
      },
      wait: (ctx) =>
        new Promise((_, reject) => {
          ctx.signal.addEventListener("abort", () => reject(ctx.signal.reason), { once: true })
        }),
    })
    rookery.jobs.handle("sample.run", {
      params: z.object({
        crash: z.boolean().optional(),
        wait: z.boolean().optional(),
        secret: z.string().optional(),
      }),
      async run(job, { crash, wait, secret }) {
        await job.log(`attempt ${job.attempt}`)
        await job.progress(1, 2)
        if (secret) {
          const { credential } = await rookery.connections.resolve(secret)
          await job.log(`the plugin wrote ${credential}`)
        }
        if (crash && job.attempt === 1) process.exit(3)
        if (wait) {
          await new Promise<void>((resolve) =>
            job.signal.addEventListener("abort", () => resolve(), { once: true }),
          )
          const reason = job.signal.reason
          // Signal reason is observable before the result is discarded by cancellation.
          await rookery.storage.set("reason", reason.details)
        }
        return { attempt: job.attempt }
      },
    })
    if (rookery.manifest.contributes.jobs.some((job) => job.id === "sample.grace")) {
      rookery.jobs.handle("sample.grace", {
        async run(job) {
          if (job.attempt === 1) {
            await new Promise<void>((resolve) =>
              job.signal.addEventListener("abort", () => resolve(), { once: true }),
            )
            await rookery.storage.set("grace", "cleaning")
            await Bun.sleep(1600)
            await rookery.storage.set("grace", "finished")
          }
          return rookery.storage.get("grace")
        },
      })
      rookery.jobs.handle("sample.kill", {
        async run(job) {
          if (job.attempt === 1) {
            await rookery.storage.set("oldPid", process.pid)
            await new Promise(() => {})
          }
          const oldPid = (await rookery.storage.get("oldPid")) as number
          try {
            process.kill(oldPid, 0)
            return { oldProcessExists: true }
          } catch (e) {
            return { oldProcessExists: (e as { code?: string }).code !== "ESRCH" }
          }
        },
      })
      rookery.jobs.handle("sample.once", {
        async run(job) {
          await rookery.storage.set("once", job.attempt)
          process.exit(4)
        },
      })
    }
  }
  rookery.rpc.handle("connections", {
    level: "own",
    params: z.object({ type: z.string().optional(), id: z.string().optional() }),
    async run(_ctx, { type, id }) {
      if (id) return rookery.connections.get(id)
      return rookery.connections.list({ type })
    },
  })
  rookery.rpc.handle("relay", {
    level: "own",
    params: z.object({ plugin: z.string(), id: z.string() }),
    async run(_ctx, { plugin, id }) {
      const name = await rookery.services.call(plugin, "sample", "describe", { id })
      const chunks = []
      for await (const chunk of rookery.services.stream(plugin, "sample", "count", { to: 2 })) {
        chunks.push(chunk)
      }
      return { name, chunks }
    },
  })
  rookery.rpc.handle("leak", {
    level: "own",
    params: z.object({ id: z.string(), plain: z.boolean().optional() }),
    async run(_ctx, { id, plain }) {
      const { credential } = await rookery.connections.resolve(id)
      console.error(`logged ${credential}`)
      console.error({ key: credential })
      if (plain) throw new Error(`exception ${credential}`)
      throw new RookeryError("INVALID", `exception ${credential}`, {
        nested: [{ [credential!]: credential }],
      })
    },
  })
  rookery.rpc.handle("claimId", { level: "own", run: () => jobCallId })
  rookery.rpc.handle("pipe", {
    level: "own",
    params: z.object({ method: z.string(), params: z.record(z.string(), z.unknown()) }),
    run: (_ctx, { method, params }) => hostCall(method, params),
  })
  rookery.rpc.handle("closeRead", {
    level: "own",
    async run() {
      const { stream } = await hostCall("host.services.open", {
        plugin: "sample",
        service: "sample",
        method: "pending",
        params: {},
      })
      const reading = hostCall("host.services.next", { stream }).catch((error) => error.code)
      await Bun.sleep(30)
      await hostCall("host.services.close", { stream })
      return reading
    },
  })
  rookery.rpc.handle("register", {
    level: "own",
    params: z.object({ type: z.enum(["job", "service"]), name: z.string() }),
    run(_ctx, { type, name }) {
      if (type === "job") rookery.jobs.handle(name, { run: () => null })
      else rookery.services.provide(name, {})
    },
  })
})
