import { defineInterface, z } from "@rookery/sdk"

export default defineInterface({
  services: {
    sample: {
      describe: { params: z.object({ id: z.string() }), result: z.string() },
      count: { params: z.object({ to: z.number() }), result: z.number(), stream: true },
      invalidResult: { params: z.object({}), result: z.string() },
      invalidChunk: { params: z.object({}), result: z.string(), stream: true },
      pending: { params: z.object({}), result: z.number(), stream: true },
      idle: { params: z.object({}), result: z.number(), stream: true, timeoutSecs: 1 },
      wait: { params: z.object({}), result: z.null(), timeoutSecs: 1 },
    },
  },
})
