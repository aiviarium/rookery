import { definePlugin, z } from "@rookery/sdk"
import api from "./interface"

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
      wait: (ctx) =>
        new Promise((_, reject) => {
          ctx.signal.addEventListener("abort", () => reject(ctx.signal.reason), { once: true })
        }),
    })
    rookery.jobs.handle("sample.run", {
      params: z.object({ crash: z.boolean().optional(), wait: z.boolean().optional() }),
      async run(job, { crash, wait }) {
        await job.log(`attempt ${job.attempt}`)
        await job.progress(1, 2)
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
  rookery.rpc.handle("register", {
    level: "own",
    params: z.object({ type: z.enum(["job", "service"]), name: z.string() }),
    run(_ctx, { type, name }) {
      if (type === "job") rookery.jobs.handle(name, { run: () => null })
      else rookery.services.provide(name, {})
    },
  })
})
