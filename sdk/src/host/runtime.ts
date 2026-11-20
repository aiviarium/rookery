// The host half's runtime. The extension host makes one in each plugin's worker, with a transport to
// the server, and hands `rookery` to the plugin's activate function. Each facility is a thin typed
// proxy over the server's methods, called as the plugin.
//
// What this version of Rookery serves to host halves: rpc, commands, events, settings, storage,
// secrets, connections, jobs (handle, enqueue, get, list, cancel, progress and log), leases,
// topics.publish, notify, services (provide and call), extensions, app services, env (on the
// invocation's behalf, see `envCall`), and data, spaces, entities, documents, users, sql, blobs,
// audit, schedules and terminals. A handle that takes the context runs for the person the invocation
// is for (see `invocationCall`), and the plugin itself when it serves none. `streams` and
// `blobs.read` still answer UNAVAILABLE: both need the server's stream channel to a host half.
import { z } from "zod"
import { type Call, collectionsProxy, createCollection, createRefs, type Row } from "../data"
import { RookeryError } from "../errors"
import type {
  AppServiceInfo,
  AuditEntry,
  BlobInfo,
  Change,
  Checkout,
  ConnectionInfo,
  ConnectionList,
  ConnectionResolved,
  DirListing,
  DocumentContent,
  DocumentConvert,
  DocumentConverted,
  DocumentCreate,
  DocumentEdit,
  DocumentEdited,
  DocumentInfo,
  EntityInfo,
  EntityTextList,
  EnvInfo,
  EnvPresence,
  ExecResult,
  ExtensionContribution,
  FileData,
  FileStat,
  FileWritten,
  Grant,
  GrantList,
  LevelHeld,
  Manifest,
  Person,
  Principal,
  Resources,
  ScheduleInfo,
  ScreenInfo,
  SpaceInfo,
  SqlExecuted,
  SqlRows,
  SqlTx,
  TerminalInfo,
  User,
} from "../generated"
import type { Interface, MethodDecls, ServiceDecl } from "../interface"
import { type Disposable, disposable } from "../types"
import type {
  Access,
  Changes,
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
  ServiceData,
  Sql,
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
  /** The space the caller's connection entered, if it entered one. */
  space?: string | null
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

/** The invocation a data call is for. `null` is the plugin itself, as in its own activate. */
interface Invocation {
  callId: string
  space: string | null
}

/** A call of a server method for one invocation, or as the plugin when there is none. */
function invocationCall(rt: HostRuntime, inv: Invocation | null): Call {
  return (method, params) =>
    inv === null
      ? rt.call(method, params)
      : rt.call<unknown>("host.data", { callId: inv.callId, method, params })
}

/**
 * The collections of this plugin, either as the plugin itself or for one invocation. An invocation's
 * calls run as the person it is for, which is how a host half reads and writes what that person may;
 * a job, or a call another plugin made, has no person and runs at the plugin's own level.
 */
function dataFor(rt: HostRuntime, inv: Invocation | null, space?: string | null): ServiceData {
  const current = space ?? inv?.space ?? null
  const bare = invocationCall(rt, inv)
  const call: Call = (method, params) => {
    const wire: Record<string, unknown> = { plugin: rt.plugin, ...params }
    if (current) wire.space = current
    return bare(method, wire)
  }
  const definition = (name: string) => rt.manifest.contributes?.collections?.find((c) => c.name === name)
  return collectionsProxy((name) => createCollection(call, name, definition), {
    refs: createRefs(call),
    changes: changesFor(rt, call),
    space: (id: string) => dataFor(rt, inv, id),
  }) as ServiceData
}

/** `data.subscribe`, one collection at a time, as `data.changed` notifications arrive. */
function changesFor(rt: HostRuntime, call: Call): Changes {
  return {
    subscribe: async (collection, handler) => {
      const started = (await call("data.subscribe", { collections: [collection] })) as {
        subscription: string
      }
      const at = started.subscription
      rt.changes.set(at, (change: Change) => {
        if (change.collection !== collection) return
        handler({
          collection: change.collection,
          op: change.op,
          id: change.id,
          row: (change.row ?? null) as Row | null,
          seq: change.seq,
        })
      })
      return disposable(() => {
        rt.changes.delete(at)
        call("data.unsubscribe", { subscription: at }).catch(() => {})
      })
    },
  }
}

const GRANTS: Record<Grant, number> = { view: 1, use: 2, manage: 3 }

/** `access.*` for one invocation: what the person it serves may do, or the plugin's own level. */
function accessFor(rt: HostRuntime, inv: Invocation): Access {
  const call = invocationCall(rt, inv)
  return {
    check: async (resource, level) => {
      const held = (await call("access.level", { resource })) as LevelHeld
      return held.level !== null && GRANTS[held.level] >= GRANTS[level]
    },
    require: async (resource, level) => {
      await call("access.require", { resource, level })
    },
    level: async (resource) => ((await call("access.level", { resource })) as LevelHeld).level,
    // Which level someone else holds is a plugin's own question, and never a person's.
    levelOf: async (resource, user) => {
      const principal = user.includes(":") ? user : `user:${user}`
      const held = (await rt.call("access.levelOf", { resource, principal })) as LevelHeld
      return held.level
    },
    filter: async (resources, level) =>
      ((await call("access.filter", { resources, level })) as Resources).resources,
    grant: async (resource, subject, level) => {
      await call("access.grant", { resource, principal: subject, level })
    },
    revoke: async (resource, subject) => {
      await call("access.revoke", { resource, principal: subject })
    },
    grants: (resource) => call("access.grants", { resource }) as Promise<GrantList>,
  }
}

/** The invocation a handler's `ctx` names. */
function asInvocation(ctx: Ctx): Invocation {
  return { callId: ctx.callId, space: ctx.space ?? null }
}

/** `sql.*`: the plugin's own schema, which no person's level changes. */
function sqlFor(rt: HostRuntime): Sql {
  const rows = async <T>(method: string, params: Record<string, unknown>) =>
    ((await rt.call(method, params)) as SqlRows).rows as T[]
  const affected = async (method: string, params: Record<string, unknown>) =>
    ((await rt.call(method, params)) as SqlExecuted).rows
  const query = <T = Record<string, unknown>>(sql: string, params: unknown[] = []) =>
    rows<T>("sql.query", { sql, params })
  return {
    query,
    execute: (sql, params = []) => affected("sql.execute", { sql, params }),
    begin: async (opts = {}) => {
      const { tx } = (await rt.call("sql.begin", opts.lease ? { lease: opts.lease } : {})) as SqlTx
      return {
        query: <T = Record<string, unknown>>(sql: string, params: unknown[] = []) =>
          rows<T>("sql.txQuery", { tx, sql, params }),
        execute: (sql, params = []) => affected("sql.txExecute", { tx, sql, params }),
        publish: async (topic, payload) => {
          await rt.call("sql.txPublish", { tx, topic, payload })
        },
        commit: async () => {
          await rt.call("sql.commit", { tx })
        },
        rollback: async () => {
          await rt.call("sql.rollback", { tx })
        },
      }
    },
  }
}

export class HostRuntime {
  readonly handlers = new Map<string, Handler>()
  readonly events = new Map<string, (payload: unknown, name: string) => unknown>()
  /** The handlers of `data.subscribe`, by subscription. */
  readonly changes = new Map<string, (change: Change) => unknown>()
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
    const space = ctx.space ?? null
    const inv: Invocation = { callId: ctx.callId, space }
    return {
      callId: ctx.callId,
      user: ctx.user ?? null,
      principal: ctx.principal,
      space,
      signal,
      data: dataFor(this, inv, space),
      access: accessFor(this, inv),
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
    if (method === "data.changed") {
      const h = this.changes.get(params?.subscription)
      if (!h) return
      if (params.ended) {
        this.changes.delete(params.subscription)
        if (params.error) {
          this.transport.log(
            "warn",
            `the change feed of ${this.plugin} ended: ${params.error.message ?? params.error}`,
          )
        }
        return
      }
      for (const change of params.changes ?? []) {
        Promise.resolve()
          .then(() => h(change))
          .catch((e) => this.transport.log("error", `a handler of the change feed failed: ${format([e])}`))
      }
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
      get: async (key, opts = {}) =>
        (await rt.call<{ value: any }>("settings.get", { plugin, key, space: opts.space })).value,
      set: async (key, value, opts = {}) => {
        await rt.call("settings.set", {
          plugin,
          key,
          value,
          scope: opts.scope ?? "default",
          space: opts.space,
        })
      },
      reset: async (key, opts = {}) => {
        await rt.call("settings.reset", { plugin, key, scope: opts.scope ?? "default", space: opts.space })
      },
    },
    storage: {
      get: async (key, opts = {}) =>
        (
          await rt.call<{ value: any; revision: number }>("storage.get", {
            key,
            scope: opts.scope ?? "global",
            space: opts.space,
          })
        ).value ?? null,
      set: async (key, value, opts = {}) => {
        await rt.call("storage.set", {
          key,
          value,
          scope: opts.scope ?? "global",
          space: opts.space,
          ...(opts.expectedRevision !== undefined ? { revision: opts.expectedRevision } : {}),
        })
      },
      list: async (opts = {}) =>
        (
          await rt.call<{ entries: Array<{ key: string; value: unknown; revision: number }> }>(
            "storage.list",
            {
              scope: opts.scope ?? "global",
              space: opts.space,
              prefix: opts.prefix ?? "",
            },
          )
        ).entries.map((e) => ({ key: e.key, value: e.value, revision: e.revision })),
      delete: async (key, opts = {}) => {
        await rt.call("storage.delete", { key, scope: opts.scope ?? "global", space: opts.space })
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
    data: dataFor(rt, null),
    spaces: {
      list: async (ctx) =>
        (
          (await invocationCall(rt, asInvocation(ctx))("spaces.list", {})) as {
            spaces: SpaceInfo[]
          }
        ).spaces,
      get: (ctx, space) =>
        invocationCall(rt, asInvocation(ctx))("spaces.get", { space }) as Promise<SpaceInfo>,
      create: (ctx, params) =>
        invocationCall(rt, asInvocation(ctx))("spaces.create", params) as Promise<SpaceInfo>,
      rename: (ctx, space, name) =>
        invocationCall(rt, asInvocation(ctx))("spaces.rename", { space, name }) as Promise<SpaceInfo>,
    },
    entities: {
      lookup: (ctx, uri) =>
        invocationCall(rt, asInvocation(ctx))("entities.resolve", { uri }) as Promise<EntityInfo | null>,
      lookupMany: async (ctx, uris) =>
        (
          (await invocationCall(rt, asInvocation(ctx))("entities.resolveMany", { uris })) as {
            entities: EntityInfo[]
          }
        ).entities,
      search: async (ctx, text, opts = {}) =>
        (
          (await invocationCall(rt, asInvocation(ctx))("entities.search", {
            text,
            ...(opts.schemes !== undefined ? { schemes: opts.schemes } : {}),
            ...(opts.space !== undefined ? { space: opts.space } : {}),
            ...(opts.limit !== undefined ? { limit: opts.limit } : {}),
          })) as { entities: EntityInfo[] }
        ).entities,
      text: (ctx, uris, opts = {}) =>
        invocationCall(rt, asInvocation(ctx))("entities.text", {
          uris,
          ...(opts.maxChars !== undefined ? { maxChars: opts.maxChars } : {}),
        }) as Promise<EntityTextList>,
      // The index of what a plugin contributed is its own to change, whoever its handler serves.
      register: (entity) => rt.call("entities.register", { ...entity }) as Promise<EntityInfo>,
      remove: async (uri) => {
        await rt.call("entities.remove", { uri })
      },
      alias: async (uri, target) => {
        await rt.call("entities.alias", { uri, target })
      },
    },
    documents: {
      list: async (ctx, parent) =>
        (
          (await invocationCall(rt, asInvocation(ctx))("documents.list", { parent })) as {
            documents: DocumentInfo[]
          }
        ).documents,
      create: (ctx, params: DocumentCreate) =>
        invocationCall(rt, asInvocation(ctx))("documents.create", params) as Promise<DocumentInfo>,
      edit: (ctx, params: DocumentEdit) =>
        invocationCall(rt, asInvocation(ctx))("documents.edit", params) as Promise<DocumentEdited>,
      delete: async (ctx, id) => {
        await invocationCall(rt, asInvocation(ctx))("documents.delete", { id })
      },
      read: (ctx, id, format) =>
        invocationCall(rt, asInvocation(ctx))("documents.read", {
          id,
          ...(format ? { format } : {}),
        }) as Promise<DocumentContent>,
      convert: (params: DocumentConvert) =>
        rt.call("documents.convert", params) as Promise<DocumentConverted>,
    },
    // A person by name, for the resource they share with whoever the handler serves.
    users: {
      search: async (ctx, resource, text, limit) =>
        (
          (await invocationCall(rt, asInvocation(ctx))("users.search", {
            resource,
            text,
            ...(limit !== undefined ? { limit } : {}),
          })) as { people: Person[] }
        ).people,
    },
    sql: sqlFor(rt),
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
    blobs: {
      put: (data, opts = {}) =>
        rt.call("blobs.put", {
          data: toBase64(data),
          contentType: opts.contentType ?? "application/octet-stream",
          ...(opts.filename !== undefined ? { name: opts.filename } : {}),
        }) as Promise<BlobInfo>,
      get: (id) => rt.call("blobs.get", { id }) as Promise<BlobInfo>,
      read: () => {
        throw new RookeryError(
          "UNAVAILABLE",
          "rookery.blobs.read needs the server's stream channel to a host half, which this version does not serve",
        )
      },
      delete: async (id) => {
        await rt.call("blobs.delete", { id })
      },
    },
    audit: {
      record: () => {
        throw new RookeryError(
          "UNAVAILABLE",
          "rookery.audit.record is not available to host halves in this version of Rookery",
        )
      },
      list: async (query = {}) => (await rt.call<{ entries: AuditEntry[] }>("audit.list", query)).entries,
    },
    schedules: {
      create: (params) => rt.call("schedules.create", params) as Promise<ScheduleInfo>,
      list: async () => (await rt.call<{ schedules: ScheduleInfo[] }>("schedules.list", {})).schedules,
      setEnabled: (id, enabled) => rt.call("schedules.setEnabled", { id, enabled }) as Promise<ScheduleInfo>,
      fire: (id, params) =>
        rt.call("schedules.fire", { id, ...(params !== undefined ? { params } : {}) }) as Promise<{
          job: string
        }>,
      remove: async (id) => {
        await rt.call("schedules.remove", { id })
      },
    },
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
    terminals: {
      list: async (ctx, env) =>
        (
          (await invocationCall(rt, asInvocation(ctx))("terminals.list", {
            ...(env ? { env } : {}),
          })) as { terminals: TerminalInfo[] }
        ).terminals,
      get: (ctx, terminal) =>
        invocationCall(rt, asInvocation(ctx))("terminals.get", { id: terminal }) as Promise<TerminalInfo>,
      screen: (ctx, terminal) =>
        invocationCall(rt, asInvocation(ctx))("terminals.screen", { id: terminal }) as Promise<ScreenInfo>,
      write: async (ctx, terminal, data) => {
        await invocationCall(rt, asInvocation(ctx))("terminals.write", { id: terminal, text: data })
      },
      close: async (ctx, terminal) => {
        await invocationCall(rt, asInvocation(ctx))("terminals.close", { id: terminal })
      },
    },
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
