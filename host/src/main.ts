// The Rookery extension host of one plugin. rookery serve starts one for each plugin whose host half
// runs, and talks JSON-RPC 2.0 with it over standard input and output, one message a line: the pipes
// say which plugin it is. It ends when its input ends, as when the server stops it.
import { RookeryError } from "@rookery/sdk/errors"
import { Host, type ServerLink } from "./host"

// Standard output is the protocol's: whatever else is printed goes to standard error, the server's log.
const stderr = (...a: unknown[]) => process.stderr.write(`${a.map(String).join(" ")}\n`)
console.log = console.info = console.debug = console.warn = console.error = stderr
process.on("uncaughtException", (e) => stderr("[host] uncaught exception:", (e as Error)?.stack ?? e))
process.on("unhandledRejection", (e) => stderr("[host] unhandled rejection:", (e as Error)?.stack ?? e))

let nextId = 1
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>()

function send(m: unknown) {
  process.stdout.write(`${JSON.stringify(m)}\n`)
}

const server: ServerLink = {
  request(method, params, plugin) {
    const id = nextId++
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject })
      send({ jsonrpc: "2.0", id, method, params, ...(plugin ? { plugin } : {}) })
    })
  },
  notify(method, params) {
    send({ jsonrpc: "2.0", method, params })
  },
}

const heap = process.argv.find((a) => a.startsWith("--heap-mb="))
const host = new Host(server, {
  heapMb: heap ? Number(heap.slice("--heap-mb=".length)) || undefined : undefined,
})

async function message(line: string) {
  let msg: any
  try {
    msg = JSON.parse(line)
  } catch {
    return
  }
  if (msg.method !== undefined && msg.id !== undefined && msg.id !== null) {
    try {
      const result = await host.handle(msg.method, msg.params ?? {})
      send({ jsonrpc: "2.0", id: msg.id, result: result ?? null })
    } catch (e) {
      send({ jsonrpc: "2.0", id: msg.id, error: RookeryError.from(e).toRpc() })
    }
  } else if (msg.method !== undefined) {
    host.notification(msg.method, msg.params ?? {})
  } else if (msg.id !== undefined) {
    const p = pending.get(msg.id)
    if (!p) return
    pending.delete(msg.id)
    if (msg.error) p.reject(RookeryError.fromRpc(msg.error))
    else p.resolve(msg.result)
  }
}

async function read() {
  const decoder = new TextDecoder()
  let buffered = ""
  for await (const chunk of process.stdin) {
    buffered += decoder.decode(chunk as Uint8Array, { stream: true })
    let at = buffered.indexOf("\n")
    while (at >= 0) {
      const line = buffered.slice(0, at)
      buffered = buffered.slice(at + 1)
      if (line.trim()) void message(line)
      at = buffered.indexOf("\n")
    }
  }
}

server
  .request("host.hello", { pid: process.pid, version: "0.1.0" })
  .then(() => stderr(`[host] ready (pid ${process.pid})`))
  .catch((e) => {
    stderr("[host] the server refused host.hello:", e)
    process.exit(1)
  })
await read()
process.exit(0)
