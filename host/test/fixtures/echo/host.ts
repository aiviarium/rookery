import { definePlugin, z } from "@rookery/sdk"

export default definePlugin((rookery) => {
  rookery.rpc.handle("greet", {
    params: z.object({ name: z.string() }),
    run: (ctx, { name }) => ({ greeting: `Hello, ${name}`, as: ctx.principal.id }),
  })
  rookery.rpc.handle("remember", {
    params: z.object({ value: z.string() }),
    run: async (_, { value }) => {
      await rookery.storage.set("last", value)
      return { stored: await rookery.storage.get("last") }
    },
  })
  rookery.rpc.handle("setting", {
    run: async () => ({ value: await rookery.settings.get("greeting" as never) }),
  })
  rookery.rpc.handle("slow", {
    run: (ctx) =>
      new Promise((resolve) => {
        const timer = setTimeout(() => resolve("late"), 60_000)
        ctx.signal.addEventListener("abort", () => clearTimeout(timer))
      }),
  })
  rookery.rpc.handle("crash", { run: () => process.exit(3) })
})
