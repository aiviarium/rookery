import { definePlugin } from "@rookery/sdk"

export default definePlugin(() => {
  setTimeout(() => process.exit(7), 10)
})
