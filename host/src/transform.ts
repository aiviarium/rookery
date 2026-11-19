// A worker that runs one transform step of a plugin's migration: the default export of
// migrations/<name>.ts, called with a `tx` whose reads and writes go into the migration's transaction
// on the server (the data.tx* methods, with the migration in their params). It ends when the function
// returns.
import { join } from "node:path"
import { parentPort, workerData } from "node:worker_threads"
import type { MigrationTx } from "@rookery/sdk"
import { RookeryError } from "@rookery/sdk/errors"
import { format } from "@rookery/sdk/runtime/host"
import type { TransformData, TransformMessage } from "./protocol"

const port = parentPort!
const t = workerData as TransformData
const post = (m: TransformMessage) => port.postMessage(m)

let nextId = 1
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>()

port.on("message", (m: { type: "result"; id: number; result?: unknown; error?: any }) => {
  const p = pending.get(m.id)
  if (!p) return
  pending.delete(m.id)
  if (m.error) p.reject(new RookeryError(m.error.code, m.error.message, m.error.details))
  else p.resolve(m.result)
})

function call(method: string, params: Record<string, unknown>): Promise<any> {
  const id = nextId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    post({
      type: "call",
      id,
      method,
      params: JSON.parse(JSON.stringify({ ...params, migration: t.migration })),
    })
  })
}

const tx: MigrationTx = {
  rows: async (collection, opts) => (await call("data.txRows", { collection, ...opts })).rows,
  async *each(collection) {
    let after: string | undefined
    for (;;) {
      const rows = await tx.rows(collection, { after, limit: 500 })
      if (!rows.length) return
      for (const r of rows) yield r
      after = rows[rows.length - 1]!.id
    }
  },
  async *legacy(table) {
    let after: string | undefined
    for (;;) {
      const page = await call("data.txLegacy", { table, after, limit: 500 })
      for (const r of page.rows as Array<Record<string, any>>) yield r
      if (!page.next || page.rows.length === 0) return
      after = page.next
    }
  },
  insert: async (collection, values, scope) =>
    (await call("data.txInsert", { collection, values, ...scope })).row,
  update: async (collection, id, patch) => (await call("data.txUpdate", { collection, id, patch })).row,
  delete: async (collection, id) => {
    await call("data.txDelete", { collection, id })
  },
  log: (...args) => post({ type: "log", level: "info", message: `[${t.name}] ${format(args)}` }),
}

// Not a top-level await: a worker's messages are dispatched only once its module has been evaluated.
async function run() {
  const mod = await import(join(t.dir, "migrations", `${t.name}.ts`))
  const fn = mod.default ?? mod[t.name]
  if (typeof fn !== "function")
    throw new RookeryError("INVALID", `migrations/${t.name}.ts must \`export default async (tx) => { ... }\``)
  await fn(tx)
}

run().then(
  () => post({ type: "done" }),
  (e) => post({ type: "failed", error: RookeryError.from(e).toBody() }),
)
