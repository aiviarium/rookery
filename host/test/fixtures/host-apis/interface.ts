import { defineInterface, z } from "@rookery/sdk"

export default defineInterface({
  services: {
    sample: {
      describe: { params: z.object({ id: z.string() }), result: z.string() },
      count: { params: z.object({ to: z.number() }), result: z.number(), stream: true },
      wait: { params: z.object({}), result: z.null(), timeoutSecs: 1 },
    },
  },
})
