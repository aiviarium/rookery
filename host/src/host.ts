// The host's main thread: it passes messages between the server and one worker for each plugin. What
// runs is the server's to decide; the host keeps no state of plugins but their workers.
import { Worker } from "node:worker_threads"
import { RookeryError } from "@rookery/sdk/errors"
import type { ErrorBody } from "@rookery/sdk/generated"
import type { Invoke, Registration } from "@rookery/sdk/runtime/host"
import type { FromWorker, ToWorker, WorkerData } from "./protocol"

/** What the host says to the server. */
export interface ServerLink {
  /** A call of a server method, as `plugin`. */
  request(method: string, params: Record<string, unknown>, plugin?: string): Promise<unknown>
  notify(method: string, params: Record<string, unknown>): void
}

/** How long a host half's cleanup may take when it is stopped. */
const DEACTIVATE_GRACE_MS = 3000

interface Slot {
  plugin: string
  worker: Worker
  waiting: Map<number, { resolve: (v: unknown) => void; reject: (e: RookeryError) => void }>
  stopping: boolean
}

export class Host {
  private readonly slots = new Map<string, Slot>()
  private nextId = 1

  constructor(private readonly server: ServerLink) {}

  plugins(): string[] {
    return [...this.slots.keys()]
  }

  /** A request of the server. */
  async handle(method: string, params: any): Promise<unknown> {
    switch (method) {
      case "plugins.activate":
        return { registrations: await this.activate(params as WorkerData) }
      case "plugins.invoke":
        return this.invoke(params as Invoke)
      case "plugins.deactivate":
        await this.stop(params.plugin, true)
        return {}
      default:
        throw new RookeryError("NOT_FOUND", `the extension host has no method ${method}`)
    }
  }

  /** A notification of the server. */
  notification(method: string, params: any) {
    if (method === "plugins.cancel") this.send(params.plugin, { type: "cancel", callId: params.callId })
    else if (method === "plugins.notify")
      this.send(params.plugin, { type: "notify", method: params.method, params: params.params })
  }

  private send(plugin: string, m: ToWorker) {
    this.slots.get(plugin)?.worker.postMessage(m)
  }

  private request(slot: Slot, m: (id: number) => ToWorker): Promise<any> {
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      slot.waiting.set(id, { resolve, reject })
      slot.worker.postMessage(m(id))
    })
  }

  async activate(data: WorkerData): Promise<Registration[]> {
    if (this.slots.has(data.plugin)) await this.stop(data.plugin, false)
    if (!data.manifest?.host) return []
    const worker = new Worker(new URL("./worker.ts", import.meta.url), {
      workerData: { plugin: data.plugin, dir: data.dir, manifest: data.manifest } satisfies WorkerData,
    })
    const slot: Slot = { plugin: data.plugin, worker, waiting: new Map(), stopping: false }
    this.slots.set(data.plugin, slot)
    worker.on("message", (m: FromWorker) => this.fromWorker(slot, m))
    worker.on("error", (e: Error) =>
      this.server.notify("plugins.log", { plugin: data.plugin, level: "error", message: `${e?.stack ?? e}` }),
    )
    worker.on("exit", (code) => this.exited(slot, code))
    return this.request(slot, (id) => ({ type: "activate", id }))
  }

  async invoke(inv: Invoke): Promise<unknown> {
    const slot = this.slots.get(inv.plugin)
    if (!slot || slot.stopping)
      throw new RookeryError("UNAVAILABLE", `the host half of ${inv.plugin} does not run`)
    return this.request(slot, (id) => ({ type: "invoke", id, invoke: inv }))
  }

  /** Stops a worker: with its cleanup first, or at once. */
  async stop(plugin: string, graceful: boolean) {
    const slot = this.slots.get(plugin)
    if (!slot) return
    slot.stopping = true
    if (graceful) {
      await Promise.race([
        this.request(slot, (id) => ({ type: "deactivate", id })).catch(() => {}),
        new Promise((r) => setTimeout(r, DEACTIVATE_GRACE_MS)),
      ])
    }
    this.slots.delete(plugin)
    this.fail(slot, new RookeryError("UNAVAILABLE", `the host half of ${plugin} was stopped`))
    await slot.worker.terminate()
  }

  private fail(slot: Slot, e: RookeryError) {
    for (const w of slot.waiting.values()) w.reject(e)
    slot.waiting.clear()
  }

  private exited(slot: Slot, code: number) {
    if (slot.stopping || this.slots.get(slot.plugin) !== slot) return
    this.slots.delete(slot.plugin)
    const error: ErrorBody = {
      code: "UNAVAILABLE",
      message: `the worker of ${slot.plugin} ended (code ${code})`,
    }
    this.fail(slot, new RookeryError(error.code, error.message))
    this.server.notify("plugins.exited", { plugin: slot.plugin, error })
  }

  private fromWorker(slot: Slot, m: FromWorker) {
    const settle = (id: number, error: ErrorBody | undefined, value: unknown) => {
      const w = slot.waiting.get(id)
      if (!w) return
      slot.waiting.delete(id)
      if (error) w.reject(new RookeryError(error.code, error.message, error.details))
      else w.resolve(value)
    }
    switch (m.type) {
      case "activated":
        return settle(m.id, undefined, m.registrations)
      case "failed":
        return settle(m.id, m.error, undefined)
      case "invoked":
        return settle(m.id, m.error, m.result ?? null)
      case "deactivated":
        return settle(m.id, undefined, null)
      case "call":
        this.server.request(m.method, m.params, slot.plugin).then(
          (result) => slot.worker.postMessage({ type: "result", id: m.id, result } satisfies ToWorker),
          (e) =>
            slot.worker.postMessage({
              type: "result",
              id: m.id,
              error: RookeryError.from(e).toBody(),
            } satisfies ToWorker),
        )
        return
      case "registered":
        this.server.notify("plugins.registered", { plugin: slot.plugin, registration: m.registration })
        return
      case "unregistered":
        this.server.notify("plugins.unregistered", { plugin: slot.plugin, kind: m.kind, key: m.key })
        return
      case "log":
        this.server.notify("plugins.log", { plugin: slot.plugin, level: m.level, message: m.message })
        return
    }
  }
}
