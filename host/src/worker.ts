// One worker for each plugin's host half. It imports the plugin's host entry and runs it with the SDK's
// host runtime, whose calls of the server go to the main thread and from there to the server.
import { join } from "node:path"
import { parentPort, workerData } from "node:worker_threads"
import { RookeryError } from "@rookery/sdk/errors"
import { activate, createHost, format, type HostTransport } from "@rookery/sdk/runtime/host"
import type { FromWorker, ToWorker, WorkerData } from "./protocol"

const port = parentPort!
const data = workerData as WorkerData
const post = (m: FromWorker) => port.postMessage(m)

let nextId = 1
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>()

/** A copy that survives JSON: functions and undefined left out. */
function clean<T>(v: T): T {
  return v === undefined ? v : JSON.parse(JSON.stringify(v))
}

const transport: HostTransport = {
  call(method, params) {
    const id = nextId++
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject })
      post({ type: "call", id, method, params: clean(params) })
    })
  },
  registered: (registration) => post({ type: "registered", registration }),
  unregistered: (kind, key) => post({ type: "unregistered", kind, key }),
  log: (level, message) => post({ type: "log", level, message }),
}

// What a plugin prints goes to the server's log, under its id.
console.log = console.info = (...a: unknown[]) => transport.log("info", format(a))
console.debug = (...a: unknown[]) => transport.log("debug", format(a))
console.warn = (...a: unknown[]) => transport.log("warn", format(a))
console.error = (...a: unknown[]) => transport.log("error", format(a))
process.on("unhandledRejection", (e) => transport.log("error", `unhandled rejection: ${format([e])}`))

const { runtime, rookery } = createHost(data.plugin, data.manifest, transport)

function body(e: unknown) {
  const err = RookeryError.from(e)
  if (err.code === "INTERNAL" && e instanceof Error && e.stack) transport.log("error", e.stack)
  return err.toBody()
}

port.on("message", async (m: ToWorker) => {
  switch (m.type) {
    case "activate":
      try {
        const mod = await import(join(data.dir, data.manifest.host))
        post({ type: "activated", id: m.id, registrations: await activate(runtime, rookery, mod) })
      } catch (e) {
        post({ type: "failed", id: m.id, error: body(e) })
      }
      return
    case "invoke":
      try {
        post({ type: "invoked", id: m.id, result: clean(await runtime.invoke(m.invoke)) })
      } catch (e) {
        post({ type: "invoked", id: m.id, error: body(e) })
      }
      return
    case "cancel":
      runtime.cancel(m.callId, m.reason)
      return
    case "notify":
      runtime.notified(m.method, m.params)
      return
    case "deactivate":
      await runtime.dispose()
      post({ type: "deactivated", id: m.id })
      return
    case "result": {
      const p = pending.get(m.id)
      if (!p) return
      pending.delete(m.id)
      if (m.error) p.reject(new RookeryError(m.error.code, m.error.message, m.error.details))
      else p.resolve(m.result)
      return
    }
  }
})
