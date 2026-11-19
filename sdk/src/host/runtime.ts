// The host half's runtime. The extension host makes one in each plugin's worker, with a transport to
// the server, and hands `rookery` to the plugin's activate function. Each facility is a thin typed
// proxy over the server's methods, called as the plugin.
//
// What this version of Rookery serves to host halves: rpc, commands, events, settings, storage,
// secrets, connections, jobs (handle, enqueue, get, list, cancel, progress and log), leases,
// topics.publish, notify, services (provide and call), extensions, app services and env (on the
// invocation's behalf, see `envCall`). The rest of the `Rookery` handle answers UNAVAILABLE.
import { z } from "zod"
import { RookeryError } from "../errors"
import type {
  AppServiceInfo,
  Checkout,
  ConnectionInfo,
  ConnectionList,
  ConnectionResolved,
  DirListing,
  EnvInfo,
  EnvPresence,
  ExecResult,
  ExtensionContribution,
  FileData,
  FileStat,
  FileWritten,
  Manifest,
  Principal,
  User,
} from "../generated"
import type { Interface, MethodDecls, ServiceDecl } from "../interface"
import { type Disposable, disposable } from "../types"
import type {
  Ctx,
  HostCommandDef,
  HostPlugin,
  JobDef,
  JobHandle,
  Lease,
  Logger,
  NoticeKind,
  Rookery,
  RpcHandlerDef,
} from "./types"
import { isHostPlugin } from "./types"

export type LogLevel = "debug" | "info" | "warn" | "error"

/** Something a host half registered, as the server knows it. */
export interface Registration {
  kind: "rpc" | "command" | "job" | "service"
  key: string
  /** For `rpc`: who may call it. */
  level?: "own" | "admin"
  params?: Record<string, unknown>
  description?: string
  methods?: Record<string, ServiceMethod>
}

interface ServiceMethod {
  stream: boolean
  timeoutSecs?: number
}

/** The context the server sends with each invocation. */
export interface CallContext {
  callId: string
  user?: User | null
  principal: Principal
  /** Milliseconds since the epoch. */
  deadline: number
}

/** `plugins.invoke`. */
export interface Invoke {
  plugin: string
  kind: Registration["kind"]
  key: string
  params: unknown
  stream?: boolean
  ctx: CallContext
}

export interface HostTransport {
  /** A call of a server method, as the plugin. */
  call(method: string, params: Record<string, unknown>): Promise<unknown>
  /** Something was registered after activation. */
  registered(registration: Registration): void
  unregistered(kind: Registration["kind"], key: string): void
  log(level: LogLevel, message: string): void
}

interface Handler {
  kind: Registration["kind"]
  key: string
  level?: Registration["level"]
  params?: { parse(input: unknown): unknown }
  description?: string
  methods?: Record<string, ServiceMethod>
  run(ctx: Ctx, params: any, invocation?: Invoke): unknown
}

export function format(args: unknown[]): string {
  return args
    .map((a) => {
      if (typeof a === "string") return a
      if (a instanceof Error) return a.stack ?? a.message
      try {
        return JSON.stringify(a)
      } catch {
        return String(a)
      }
    })
    .join(" ")
}

function jsonSchema(params: unknown): Record<string, unknown> | undefined {
  if (!params) return undefined
  try {
    return z.toJSONSchema(params as z.ZodType) as Record<string, unknown>
  } catch {
    return undefined
  }
}

/** An object whose every member answers UNAVAILABLE, for what this version does not serve. */
function unavailable<T extends object>(name: string): T {
  const refuse = () => {
    throw new RookeryError(
      "UNAVAILABLE",
      `rookery.${name} is not available to host halves in this version of Rookery`,
    )
  }
  return new Proxy({} as T, {
    get(_, member) {
      if (member === "then" || typeof member === "symbol") return undefined
      return refuse
    },
  })
}

/** A handler's params, read with its schema: what does not fit is INVALID. */
function parse(h: Handler, input: unknown): unknown {
  return h.params ? parseWith(h.params, `${h.kind} ${h.key}`, input) : input
}

function parseWith(schema: { parse(input: unknown): unknown }, what: string, input: unknown): unknown {
  try {
    return schema.parse(input ?? {})
  } catch (e) {
    const issues = (e as { issues?: unknown })?.issues
    throw new RookeryError(
      "INVALID",
      `params of ${what}: ${(e as Error)?.message ?? e}`,
      issues ? { issues } : undefined,
    )
  }
}

/** Bytes, or text as UTF-8, in base64. */
function toBase64(content: string | Uint8Array): string {
  const bytes = typeof content === "string" ? new TextEncoder().encode(content) : content
  let binary = ""
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  }
  return btoa(binary)
}

/**
 * An env method of the invocation `ctx` names. The server runs it as the person the invocation is
 * for, or as the plugin when there is none, so a host half never chooses who it acts as.
 */
function envCall<T>(rt: HostRuntime, ctx: Ctx, method: string, params: Record<string, unknown>): Promise<T> {
  return rt.call<T>("host.env", { callId: ctx.callId, method, params })
}

export class HostRuntime {
  readonly handlers = new Map<string, Handler>()
  readonly events = new Map<string, (payload: unknown, name: string) => unknown>()
  readonly disposers: Array<() => unknown> = []
  readonly running = new Map<string, AbortController>()
  active = false
  interface?: Interface

  constructor(
    readonly plugin: string,
    readonly manifest: Manifest,
    readonly transport: HostTransport,
  ) {}

  logger(): Logger {
    const at = (level: LogLevel) => (message: string, fields?: Record<string, unknown>) =>
      this.transport.log(level, fields ? `${message} ${format([fields])}` : message)
    return { debug: at("debug"), info: at("info"), warn: at("warn"), error: at("error") }
  }

  call<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    return this.transport.call(method, params) as Promise<T>
  }

  registration(h: Handler): Registration {
    const r: Registration = { kind: h.kind, key: h.key }
    if (h.level) r.level = h.level
    const params = jsonSchema(h.params)
    if (params) r.params = params
    if (h.description) r.description = h.description
    if (h.methods) r.methods = h.methods
    return r
  }

  register(h: Handler): Disposable {
    const id = `${h.kind}:${h.key}`
    if (this.handlers.has(id)) {
      throw new RookeryError("CONFLICT", `${h.kind} ${h.key} is registered already by ${this.plugin}`)
    }
    this.handlers.set(id, h)
    if (this.active) this.transport.registered(this.registration(h))
    return disposable(() => {
      if (this.handlers.get(id) !== h) return
      this.handlers.delete(id)
      if (this.active) this.transport.unregistered(h.kind, h.key)
    })
  }

  registrations(): Registration[] {
    return [...this.handlers.values()].map((h) => this.registration(h))
  }

  context(ctx: CallContext, signal: AbortSignal): Ctx {
    return {
      callId: ctx.callId,
      user: ctx.user ?? null,
      principal: ctx.principal,
      space: null,
      signal,
      data: unavailable("data"),
      access: unavailable("access"),
    } as Ctx
  }

  /** Runs a handler for `plugins.invoke`, until its deadline or `plugins.cancel`. */
  async invoke(inv: Invoke): Promise<unknown> {
    const h = this.handlers.get(`${inv.kind}:${inv.key}`)
    if (!h) throw new RookeryError("NOT_FOUND", `${this.plugin} has no ${inv.kind} ${inv.key}`)
    const abort = new AbortController()
    const timer = setTimeout(
      () => abort.abort(new RookeryError("TIMEOUT", "the call's deadline passed")),
      Math.max(0, inv.ctx.deadline - Date.now()),
    )
    this.running.set(inv.ctx.callId, abort)
    try {
      if (inv.ctx.deadline <= Date.now()) {
        throw new RookeryError("TIMEOUT", "the call's deadline passed")
      }
      const params = parse(h, inv.params)
      const result = await h.run(this.context(inv.ctx, abort.signal), params, inv)
      // Acknowledging cancellation tells the server that the handler has stopped.
      abort.signal.throwIfAborted()
      return result ?? null
    } finally {
      clearTimeout(timer)
      this.running.delete(inv.ctx.callId)
    }
  }

  /** `plugins.cancel`: the deadline passed (`timeout`), or someone cancelled the job (`cancelled`). */
  cancel(callId: string, reason?: string) {
    const error =
      reason === "timeout" || reason === undefined
        ? new RookeryError("TIMEOUT", "the call's deadline passed")
        : new RookeryError("UNAVAILABLE", "the call was cancelled", { reason })
    this.running.get(callId)?.abort(error)
  }

  /** `plugins.notify`: what the server sent the plugin's connection. */
  notified(method: string, params: any) {
    if (method === "events.event") {
      const h = this.events.get(params?.subscription)
      if (!h) return
      Promise.resolve()
        .then(() => h(params.payload, params.name))
        .catch((e) =>
          this.transport.log("error", `the handler of the event ${params.name} failed: ${format([e])}`),
        )
    }
  }

  async dispose() {
    for (const d of this.disposers.splice(0).reverse()) {
      try {
        await d()
      } catch (e) {
        this.transport.log("error", `a cleanup failed: ${format([e])}`)
      }
    }
    for (const a of this.running.values()) a.abort(new RookeryError("UNAVAILABLE", "the plugin is stopping"))
  }
}

function createApi(rt: HostRuntime): Rookery {
  const plugin = rt.plugin
  const lease = (key: string, info: { fence: number; expiresAt?: string | null }): Lease => ({
    key,
    token: info.fence,
    expiresAt: info.expiresAt ?? "",
    renew: (ttlMs = 30_000) =>
      rt.call("leases.renew", { name: key, ttlMs, fence: info.fence }) as Promise<any>,
    release: async () => {
      await rt.call("leases.release", { name: key, fence: info.fence })
    },
  })
  const api: Rookery = {
    id: plugin,
    manifest: rt.manifest,
    log: rt.logger(),
    commands: {
      register: (id, def: HostCommandDef) =>
        rt.register({ kind: "command", key: id, params: def.params, run: def.run }),
      run: async (ctx, id, params = {}) => {
        const h = rt.handlers.get(`command:${id}`)
        if (!h) throw new RookeryError("NOT_FOUND", `${plugin} has no command ${id}`)
        return (await h.run(ctx, parse(h, params))) as any
      },
    },
    rpc: {
      handle: (method, def: RpcHandlerDef) => {
        if (def.level !== "own" && def.level !== "admin")
          throw new RookeryError("INVALID", `rpc ${method} needs a level: "own" or "admin"`)
        return rt.register({ kind: "rpc", key: method, level: def.level, params: def.params, run: def.run })
      },
    },
    events: {
      publish: async (topic, payload) => {
        await rt.call("events.publish", { name: topic, payload: payload ?? null })
      },
      // A name without a `/` is this plugin's own; one with it, such as `rookery/hosts.changed`, is taken
      // as it is.
      subscribe: async (topic: string, handler: (payload: any, name: string) => unknown) => {
        const name = topic.includes("/") ? topic : `${plugin}/${topic}`
        const { subscription } = await rt.call<{ subscription: string }>("events.subscribe", {
          names: [name],
        })
        rt.events.set(subscription, handler)
        return disposable(() => {
          rt.events.delete(subscription)
          rt.call("events.unsubscribe", { subscription }).catch(() => {})
        })
      },
    },
    settings: {
      get: async (key) => (await rt.call<{ value: any }>("settings.get", { plugin, key })).value,
      set: async (key, value, opts = {}) => {
        await rt.call("settings.set", { plugin, key, value, scope: opts.scope ?? "default" })
      },
      reset: async (key, opts = {}) => {
        await rt.call("settings.reset", { plugin, key, scope: opts.scope ?? "default" })
      },
    },
    storage: {
      get: async (key, opts = {}) =>
        (
          await rt.call<{ value: any; revision: number }>("storage.get", {
            key,
            scope: opts.scope ?? "global",
          })
        ).value ?? null,
      set: async (key, value, opts = {}) => {
        await rt.call("storage.set", {
          key,
          value,
          scope: opts.scope ?? "global",
          ...(opts.expectedRevision !== undefined ? { revision: opts.expectedRevision } : {}),
        })
      },
      list: async (opts = {}) =>
        (
          await rt.call<{ entries: Array<{ key: string; value: unknown; revision: number }> }>(
            "storage.list",
            {
              scope: opts.scope ?? "global",
              prefix: opts.prefix ?? "",
            },
          )
        ).entries.map((e) => ({ key: e.key, value: e.value, revision: e.revision })),
      delete: async (key, opts = {}) => {
        await rt.call("storage.delete", { key, scope: opts.scope ?? "global" })
      },
    },
    secrets: {
      get: async (name, opts = {}) =>
        (
          await rt.call<{ value: string | null }>("secrets.get", {
            plugin,
            name,
            scope: opts.scope ?? "global",
          })
        ).value ?? null,
      set: async (name, value, opts = {}) => {
        await rt.call("secrets.set", { plugin, name, value, scope: opts.scope ?? "global" })
      },
      list: async () =>
        (await rt.call<{ secrets: Array<{ name: string }> }>("secrets.list", { plugin })).secrets.map(
          (s) => s.name,
        ),
      delete: async (name, opts = {}) => {
        await rt.call("secrets.delete", { plugin, name, scope: opts.scope ?? "global" })
      },
      rename: async (from, to, opts = {}) => {
        await rt.call("secrets.rename", { plugin, name: from, newName: to, scope: opts.scope ?? "global" })
      },
    },
    jobs: {
      // A job the manifest declares (`contributes.jobs`), run here when the server runs one of its kind.
      handle: (name, def: JobDef) => {
        if (!rt.manifest.contributes?.jobs?.some((job) => job.id === name && !job.worker)) {
          throw new RookeryError("FORBIDDEN", `${plugin} declares no host job ${name}`)
        }
        return rt.register({
          kind: "job",
          key: name,
          run: (
            ctx,
            p: {
              params?: unknown
              job: { id: string; attempt: number; maxAttempts: number; kind: string; subject?: string | null }
            },
            invocation,
          ) => {
            const callId = invocation?.ctx.callId
            const params = def.params ? parseWith(def.params, `job ${name}`, p.params ?? {}) : p.params
            const handle: JobHandle = {
              id: p.job.id,
              attempt: p.job.attempt,
              signal: ctx.signal,
              ctx,
              context: {
                id: p.job.id,
                name: p.job.kind,
                attempt: p.job.attempt,
                maxAttempts: p.job.maxAttempts,
              },
              progress: async (done, total, message) => {
                await rt.call("host.job.progress", {
                  callId,
                  progress: {
                    done,
                    ...(total !== undefined ? { total } : {}),
                    ...(message ? { message } : {}),
                  },
                })
              },
              log: async (message, level = "info") => {
                await rt.call("host.job.log", { callId, level, text: message })
              },
            }
            return def.run(handle, params)
          },
        })
      },
      enqueue: (name, params = {}) => {
        const declared = rt.manifest.contributes?.jobs?.some((job) => job.id === name)
        const ownPrefix = `${plugin}.`
        const kind =
          declared && name.startsWith(ownPrefix) ? `${plugin}/${name.slice(ownPrefix.length)}` : name
        return rt.call("jobs.enqueue", { kind, params })
      },
      get: (id) => rt.call("jobs.get", { id }) as Promise<any>,
      cancel: async (id) => {
        await rt.call("jobs.cancel", { id })
      },
    },
    leases: {
      acquire: async (key, opts = {}) => {
        try {
          const got = await rt.call<{ fence: number; expiresAt?: string }>("leases.acquire", {
            name: key,
            ttlMs: opts.ttlMs ?? 30_000,
          })
          return lease(key, got)
        } catch (e) {
          if (RookeryError.from(e).code === "CONFLICT") return null
          throw e
        }
      },
      with: async (key, fn, opts = {}) => {
        const held = await api.leases.acquire(key, opts)
        if (!held) throw new RookeryError("CONFLICT", `the lease ${key} is held by another worker`)
        try {
          return await fn(held)
        } finally {
          await held.release().catch(() => {})
        }
      },
    },
    topics: {
      publish: (topic, payload) => rt.call("topics.publish", { topic, payload }) as Promise<{ seq: number }>,
      subscribe: () => {
        throw new RookeryError(
          "UNAVAILABLE",
          "rookery.topics.subscribe is not available to host halves in this version of Rookery",
        )
      },
    },
    notify: {
      send: async (notice) => {
        const kind: NoticeKind = notice.kind ?? "info"
        const sent = await rt.call<{ notification: { id: string } }>("notify.send", {
          user: notice.user,
          kind,
          title: notice.title,
          ...(notice.body !== undefined ? { body: notice.body } : {}),
          ...(notice.resource !== undefined ? { data: { resource: notice.resource } } : {}),
        })
        return { id: sent.notification.id }
      },
    },
    services: {
      provide: (service: string | ServiceDecl, methods: Record<string, (ctx: Ctx, params: any) => unknown>) =>
        provide(rt, service, methods),
      call: (provider, service, method, params = {}) =>
        rt.call("services.call", { plugin: provider, service, method, params }),
      stream: (provider, service, method, params = {}) =>
        streamService(rt, { plugin: provider, service, method, params }),
    },
    extensions: {
      list: async (point) =>
        (await rt.call<{ contributions: ExtensionContribution[] }>("extensions.list", { point }))
          .contributions,
    },
    data: unavailable("data"),
    spaces: unavailable("spaces"),
    entities: unavailable("entities"),
    documents: unavailable("documents"),
    users: unavailable("users"),
    sql: unavailable("sql"),
    connections: {
      list: async (opts = {}) => {
        if (opts.space !== undefined) {
          throw new RookeryError("UNAVAILABLE", "space-scoped connections are unavailable")
        }
        return (await rt.call<ConnectionList>("connections.list", { type: opts.type })).connections
      },
      get: (id) => rt.call<ConnectionInfo>("connections.get", { id }),
      resolve: (id) => rt.call<ConnectionResolved>("connections.resolve", { id }),
    },
    blobs: unavailable("blobs"),
    audit: unavailable("audit"),
    schedules: unavailable("schedules"),
    streams: unavailable("streams"),
    // Every env method goes through the invocation the handler serves: the server runs it for that
    // person, or as the plugin when the invocation has no person, as in a job.
    env: {
      list: async (ctx) => (await envCall<{ environments: EnvInfo[] }>(rt, ctx, "env.list", {})).environments,
      get: (ctx, env) => envCall<EnvInfo>(rt, ctx, "env.get", { env }),
      presence: (ctx, env) => envCall<EnvPresence>(rt, ctx, "env.presence", { env }),
      exec: (ctx, env, command, opts = {}) =>
        envCall<ExecResult>(rt, ctx, "env.exec", {
          env,
          command,
          ...(opts.cwd !== undefined ? { cwd: opts.cwd } : {}),
          ...(opts.env !== undefined ? { vars: opts.env } : {}),
          ...(opts.stdin !== undefined ? { stdin: opts.stdin } : {}),
          ...(opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : {}),
        }),
      checkouts: async (ctx, env) =>
        (await envCall<{ checkouts: Checkout[] }>(rt, ctx, "env.checkouts", { env })).checkouts,
      clone: (ctx, env, repository, opts = {}) =>
        envCall<Checkout>(rt, ctx, "env.clone", { env, repository, ...opts }),
      worktree: (ctx, env, checkout, branch, opts = {}) =>
        envCall<Checkout>(rt, ctx, "env.worktree", { env, checkout, branch, ...opts }),
      removeCheckout: async (ctx, env, path, opts = {}) => {
        await envCall(rt, ctx, "env.removeCheckout", { env, path, ...opts })
      },
      readFile: (ctx, env, path) => envCall<FileData>(rt, ctx, "env.readFile", { env, path }),
      writeFile: (ctx, env, path, content, opts = {}) =>
        envCall<FileWritten>(rt, ctx, "env.writeFile", {
          env,
          path,
          base64: toBase64(content),
          ...opts,
        }),
      deleteFile: async (ctx, env, path, opts = {}) => {
        await envCall(rt, ctx, "env.deleteFile", { env, path, ...opts })
      },
      listDir: (ctx, env, path) => envCall<DirListing>(rt, ctx, "env.listDir", { env, path }),
      stat: (ctx, env, path) => envCall<FileStat>(rt, ctx, "env.stat", { env, path }),
    },
    terminals: unavailable("terminals"),
    appServices: {
      list: async () => (await rt.call<{ services: AppServiceInfo[] }>("services.list", { plugin })).services,
      // A service of `provides.services` one of this plugin's app services implements.
      call: (service, method, params = {}) =>
        rt.call("services.call", { plugin, service, method, params }) as Promise<any>,
    },
    onDispose: (fn) => {
      rt.disposers.push(fn)
    },
  }
  return api
}

function provide(
  rt: HostRuntime,
  requested: string | ServiceDecl,
  methods: Record<string, (ctx: Ctx, params: any) => unknown>,
): Disposable {
  const name = typeof requested === "string" ? requested : requested.name
  if (!rt.manifest.provides?.services?.includes(name)) {
    throw new RookeryError("FORBIDDEN", `${rt.plugin} declares no service ${name}`)
  }
  const service = rt.interface?.services[name]
  if (rt.interface && !service) {
    throw new RookeryError("FORBIDDEN", `the interface of ${rt.plugin} declares no service ${name}`)
  }
  const declarations: MethodDecls | undefined =
    service?.methods ?? (typeof requested === "string" ? undefined : requested.methods)
  if (
    declarations &&
    (Object.keys(declarations).some(
      (method) => typeof methods[method] !== "function" || !Object.hasOwn(methods, method),
    ) ||
      Object.keys(methods).some((method) => !Object.hasOwn(declarations, method)))
  ) {
    throw new RookeryError("INVALID", `the implementation of ${name} must match its interface methods`)
  }
  const handlers = Object.entries(methods)
  return rt.register({
    kind: "service",
    key: name,
    methods: Object.fromEntries(
      handlers.map(([method]) => [
        method,
        { stream: declarations?.[method]?.stream === true, timeoutSecs: declarations?.[method]?.timeoutSecs },
      ]),
    ),
    run: async (ctx, p: { method: string; params?: unknown }, invocation) => {
      const handler = Object.hasOwn(methods, p.method) ? methods[p.method] : undefined
      if (typeof handler !== "function")
        throw new RookeryError("NOT_FOUND", `the service ${name} of ${rt.plugin} has no method ${p.method}`)
      const decl = declarations?.[p.method]
      if (Boolean(invocation?.stream) !== Boolean(decl?.stream))
        throw new RookeryError(
          "INVALID",
          `use ${decl?.stream ? "services.stream" : "services.call"} for ${name}.${p.method}`,
        )
      const params = decl ? parseWith(decl.params, `${name}.${p.method}`, p.params) : (p.params ?? {})
      const result = await handler(ctx, params)
      const checked = (value: unknown) => {
        try {
          return decl ? decl.result.parse(value) : value
        } catch {
          throw new RookeryError("INTERNAL", `the result of ${name}.${p.method} does not match its interface`)
        }
      }
      if (!invocation?.stream) return checked(result)
      for await (const item of result as AsyncIterable<unknown>) {
        ctx.signal.throwIfAborted()
        await rt.call("host.stream.item", { callId: invocation.ctx.callId, item: checked(item) ?? null })
      }
      return null
    },
  })
}

async function* streamService<T>(rt: HostRuntime, params: Record<string, unknown>): AsyncGenerator<T> {
  const { stream } = await rt.call<{ stream: string }>("host.services.open", params)
  try {
    for (;;) {
      const next = await rt.call<{ done: boolean; item: T }>("host.services.next", { stream })
      if (next.done) return
      yield next.item
    }
  } finally {
    await rt.call("host.services.close", { stream }).catch(() => {})
  }
}

/** Makes the runtime and the `rookery` handle of one plugin. */
export function createHost(plugin: string, manifest: Manifest, transport: HostTransport) {
  const runtime = new HostRuntime(plugin, manifest, transport)
  return { runtime, rookery: createApi(runtime) }
}

/** Activates a plugin module's default export, and gives what it registered. */
export async function activate(
  runtime: HostRuntime,
  rookery: Rookery,
  mod: unknown,
): Promise<Registration[]> {
  const plugin = (mod as { default?: unknown })?.default ?? mod
  if (!isHostPlugin(plugin)) {
    throw new RookeryError(
      "INVALID",
      `the host half of ${runtime.plugin} must \`export default definePlugin((rookery) => { ... })\``,
    )
  }
  const out = await (plugin as HostPlugin).activate(rookery)
  if (typeof out === "function") runtime.disposers.push(out as () => unknown)
  runtime.active = true
  return runtime.registrations()
}
