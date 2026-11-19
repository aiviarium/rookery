// Assembles the client half's `rookery` handle for a plugin from a ClientScope.
import type { Component, JSX } from "solid-js"
import {
  type Call,
  type Collection,
  collectionsProxy,
  createCollection,
  createRefs,
  type Query,
  type Refs,
  type Row,
} from "../data"
import { RookeryError } from "../errors"
import type {
  AppServiceInfo,
  AppServiceList,
  AppServiceStatus,
  BlobInfo,
  CollectionContribution,
  CollectionDisplay,
  CommandInfo,
  CommandList,
  ConnectionCreate,
  ConnectionInfo,
  ConnectionTested,
  ConnectionType,
  ConnectionUpdate,
  Delivered,
  DirListing,
  EnvInfo,
  EnvPresence,
  EnvSession,
  ExecResult,
  ExtensionContribution,
  ExtensionList,
  FileData,
  FileStat,
  FileWritten,
  JobEnd,
  JobInfo,
  JobQuery,
  LogItem,
  LogItems,
  Manifest,
  NotificationInfo,
  NotificationList,
  NotifySend,
  NotifySent,
  PortUrl,
  Revision,
  Scope,
  ScreenInfo,
  ScreenQuery,
  Setting,
  SettingScope,
  SpaceInfo,
  SpaceList,
  StorageEntry,
  TerminalAttach,
  TerminalInfo,
  TerminalOpen,
  TerminalQuery,
} from "../generated"
import {
  type Collections,
  type CommandId,
  type Disposable,
  disposable,
  type SettingKey,
  type Settings as TypedSettings,
  type ViewId,
} from "../types"
import {
  type NotificationMethod,
  type NotificationParams,
  type WireMethod,
  type WireParams,
  type WireResult,
  wireCall,
} from "../wire"
import { createLive, type Live } from "./live"
import type {
  ChromeComponentProps,
  ClientCommandDef,
  ClientScope,
  ClientStream,
  ContextProvider,
  EmbeddedViewProps,
  EventHandler,
  EventSubscriptionOptions,
  FieldParts,
  NavSection,
  OpenViewOptions,
  RecordSectionProps,
  StreamOptions,
  Toast,
  ViewComponent,
  ViewOptions,
} from "./runtime"

export interface Views {
  register(id: ViewId, component: ViewComponent, options?: ViewOptions): Disposable
  field(collection: string, field: string, parts: FieldParts): Disposable
  section(view: ViewId, id: string, component: (props: RecordSectionProps) => JSX.Element): Disposable
  readonly View: Component<EmbeddedViewProps> | undefined
  href(id: ViewId, opts?: OpenViewOptions): string | null
}

export interface Commands {
  register(id: CommandId, def: ClientCommandDef): Disposable
  run<T = unknown>(id: CommandId, params?: Record<string, unknown>): Promise<T>
  list(): Promise<CommandInfo[]>
}

export interface Nav {
  section(id: string, section: NavSection): Disposable
  badge(entry: string, value: () => string | number | undefined): Disposable
}

export interface Chrome {
  component(id: string, component: (props: ChromeComponentProps) => JSX.Element): Disposable
}

export interface Workbench {
  openView(id: ViewId, opts?: OpenViewOptions): void
  href(id: ViewId, opts?: OpenViewOptions): string | null
  toast(t: Toast): void
  statusItem(id: string, component: Component): Disposable
}

export interface Events {
  /**
   * Subscribes to events of this plugin by name, or to any event of the server by its full name.
   *
   * A name without a `/` is this plugin's own, so `hosts.changed` subscribes to `fleet/hosts.changed`;
   * a name with one is taken as it is, such as `fleet/*` for everything the fleet plugin publishes.
   * Events are at most once: when the connection is too slow for some, `onDropped` says how many and
   * the subscriber reads again what it shows, or subscribes again if it needs what it missed.
   */
  subscribe(
    names: string | readonly string[],
    handler: EventHandler,
    opts?: EventSubscriptionOptions,
  ): Promise<Disposable>
  /** Publishes an event of this plugin, `payload` being what subscribers receive. */
  publish(name: string, payload?: unknown): Promise<Delivered>
}

/**
 * The notifications of the server a plugin may hear, by name and typed from the wire. The server
 * sends `events.event`, `events.dropped` and `notify.new` today; `notify.changed` is declared for the
 * server code that will send it. Changes of hosts, resources and jobs are events of the server
 * (`ServerEvents`), which `events.subscribe` takes by their names, such as `rookery/jobs.changed`.
 */
export interface Notifications {
  on<M extends NotificationMethod>(method: M, fn: (params: NotificationParams<M>) => void): Disposable
}

/** The caller's notifications: the inbox the shell's bell shows. */
export interface Notify {
  list(query?: { unread?: boolean; before?: string; limit?: number }): Promise<NotificationList>
  /** Marks the notifications given, or with none every one of the caller's, read. Returns the count. */
  read(ids?: string[]): Promise<number>
  answer(id: string, answer: string): Promise<NotificationInfo>
  /** Mutes or unmutes a kind of notification for the caller. */
  mute(kind: string, muted: boolean): Promise<void>
  /** Puts a notification of this plugin in a person's inbox. */
  send(input: Omit<NotifySend, "plugin">): Promise<NotifySent>
}

export interface LiveCollection<T = Row> extends Collection<T> {
  live(query?: Omit<Query<T>, "after">): Live<T>
}

export interface Showable {
  plugin: string
  collection: string
  display: CollectionDisplay
  def: CollectionContribution
  scheme?: string
  entityView?: string
}

export type Data = { readonly [K in keyof Collections]: LiveCollection<Collections[K]> } & {
  readonly refs: Refs
  space(id: string): Data
  collection<T = Row>(plugin: string, name: string): LiveCollection<T>
  showable(): Showable[]
}

export interface Streams {
  open(method: string, params?: Record<string, unknown>, opts?: StreamOptions): Promise<ClientStream>
  channel(channel: string, params?: unknown, opts?: StreamOptions): Promise<ClientStream>
}

export interface Host {
  call<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T>
  stream(method: string, params?: Record<string, unknown>, opts?: StreamOptions): Promise<ClientStream>
}

export interface Rpc {
  call<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T>
}

export interface Settings {
  /** A setting of this plugin as it resolves for the caller: their own, else the global one, else the default. */
  get<K extends SettingKey>(key: K): Promise<TypedSettings[K]>
  /** Sets a value at one scope: the default or the global value needs an admin. */
  set<K extends SettingKey>(key: K, value: TypedSettings[K], scope: SettingScope): Promise<void>
  /** Every setting of this plugin, as it resolves for the caller. */
  list(): Promise<Setting[]>
  reset(key: SettingKey, scope: SettingScope): Promise<void>
}

export interface Storage {
  get<T = unknown>(key: string, opts?: { scope?: Scope }): Promise<T | null>
  set(key: string, value: unknown, opts?: { scope?: Scope; revision?: number }): Promise<Revision>
  list(opts: { prefix: string; scope?: Scope; after?: string; limit?: number }): Promise<StorageEntry[]>
  delete(key: string, opts?: { scope?: Scope; revision?: number }): Promise<boolean>
}

/** A job's log as it arrives: one line at a time, then how the job ended. */
export interface JobFollower {
  dispose(): void
}

export interface Jobs {
  enqueue(kind: string, params?: unknown, opts?: { subject?: string }): Promise<JobInfo>
  get(id: string): Promise<JobInfo>
  list(query?: JobQuery): Promise<JobInfo[]>
  /** Cancels a job. Queued at once, running by asking it to stop. */
  cancel(id: string): Promise<JobInfo>
  retry(id: string): Promise<JobInfo>
  /** The log after a cursor, at most 1000 items, and the job's state. */
  output(id: string, opts?: { after?: number }): Promise<LogItems>
  /**
   * Follows a job's log from a cursor: `onItem` for each line, then `onDone` with how it ended.
   */
  follow(
    id: string,
    onItem: (item: LogItem) => void,
    opts?: { after?: number; onDone?: (end: JobEnd) => void },
  ): Promise<JobFollower>
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

export interface ClientEnv {
  list(): Promise<EnvInfo[]>
  get(env: string): Promise<EnvInfo>
  presence(env: string): Promise<EnvPresence>
  exec(
    env: string,
    command: string[],
    opts?: { cwd?: string; env?: Record<string, string>; timeoutMs?: number },
  ): Promise<ExecResult>
  readFile(env: string, path: string): Promise<FileData>
  /** Writes bytes, or text as UTF-8. With `ifMatch`, only while the file is at that SHA-256. */
  writeFile(
    env: string,
    path: string,
    content: string | Uint8Array,
    opts?: { ifMatch?: string; overwrite?: boolean },
  ): Promise<FileWritten>
  deleteFile(env: string, path: string, opts?: { recursive?: boolean }): Promise<void>
  listDir(env: string, path: string): Promise<DirListing>
  stat(env: string, path: string): Promise<FileStat>
  connect(env: string, port: number, opts?: StreamOptions): Promise<ClientStream>
  /** An address that opens a port in a browser, on the ports' own origin. It works once. */
  portUrl(env: string, port: number, path?: string): Promise<PortUrl>
}

export interface ClientAgents {
  connect(env: string, opts?: StreamOptions): Promise<ClientStream>
}

export interface ClientTerminals {
  open(env: string, opts?: Omit<TerminalOpen, "env">): Promise<TerminalInfo>
  /** The stream of a terminal; its `result` is the `TerminalAttached`. */
  attach(id: string, opts?: Omit<TerminalAttach, "id">, stream?: StreamOptions): Promise<ClientStream>
  write(id: string, data: string): Promise<void>
  resize(id: string, cols: number, rows: number): Promise<void>
  signal(id: string, signal: string): Promise<void>
  rename(id: string, title: string): Promise<void>
  close(id: string): Promise<void>
  list(opts?: TerminalQuery): Promise<TerminalInfo[]>
  get(id: string): Promise<TerminalInfo>
  screen(id: string, opts?: Omit<ScreenQuery, "id">): Promise<ScreenInfo>
  sessions(env: string, opts?: { preview?: boolean }): Promise<EnvSession[]>
  adopt(env: string, session: string): Promise<TerminalInfo>
}

export interface Connections {
  types(): Promise<ConnectionType[]>
  list(opts?: { type?: string }): Promise<ConnectionInfo[]>
  get(id: string): Promise<ConnectionInfo>
  create(input: ConnectionCreate): Promise<ConnectionInfo>
  update(input: ConnectionUpdate): Promise<ConnectionInfo>
  delete(id: string): Promise<void>
  test(id: string): Promise<ConnectionTested>
}

export interface Spaces {
  current(): SpaceInfo | null
  list(): Promise<SpaceInfo[]>
  get(id: string): Promise<SpaceInfo>
  create(params: { kind: string; name: string }): Promise<SpaceInfo>
  rename(id: string, name: string): Promise<SpaceInfo>
  enter(id: string): Promise<void>
}

export interface Contexts {
  provide(id: string, provider: ContextProvider): Disposable
  value(id: string): string | undefined
  switch(id: string, value: string): void
}

export interface Blobs {
  get(id: string): Promise<BlobInfo>
  /**
   * The URL a shell serves a blob from. `rookery serve` has no such route: it reads a blob's bytes
   * with the `blobs.read` stream, which this handle does not wrap yet.
   */
  url(id: string): string
}

export interface Extensions {
  list(point: string): Promise<ExtensionContribution[]>
}

export interface Plugins {
  call<T = unknown>(plugin: string, method: string, params?: Record<string, unknown>): Promise<T>
  stream(
    plugin: string,
    method: string,
    params?: Record<string, unknown>,
    opts?: StreamOptions,
  ): Promise<ClientStream>
}

export interface AppServices {
  url(service: string, path?: string): string
  list(): Promise<AppServiceInfo[]>
  status(service: string): Promise<AppServiceStatus>
}

export interface ClientRookery {
  readonly id: string
  readonly manifest: Manifest
  readonly views: Views
  readonly commands: Commands
  readonly nav: Nav
  readonly chrome: Chrome
  readonly workbench: Workbench
  readonly events: Events
  readonly notifications: Notifications
  readonly notify: Notify
  readonly data: Data
  readonly streams: Streams
  readonly host: Host
  readonly rpc: Rpc
  readonly settings: Settings
  readonly storage: Storage
  readonly jobs: Jobs
  readonly agents: ClientAgents
  readonly env: ClientEnv
  readonly terminals: ClientTerminals
  readonly connections: Connections
  readonly spaces: Spaces
  readonly contexts: Contexts
  readonly blobs: Blobs
  readonly extensions: Extensions
  readonly plugins: Plugins
  readonly appServices: AppServices
}

function createData(s: ClientScope, space?: string): Data {
  const currentSpace = () => space ?? s.rt.space?.()?.id ?? null
  const call: Call = (method, params) => {
    const sp = currentSpace()
    return s.rt.call(method, { plugin: s.plugin, ...(sp ? { space: sp } : {}), ...params })
  }
  const manifestOf = (plugin: string) =>
    plugin === s.plugin ? s.manifest : s.rt.manifests?.().find((m) => m.id === plugin)
  const collectionOf = (plugin: string, name: string): LiveCollection<any> => {
    const collCall: Call = (method, params) => {
      const sp = currentSpace()
      return s.rt.call(method, { plugin, ...(sp ? { space: sp } : {}), ...params })
    }
    const base = createCollection(collCall, name, (n) =>
      manifestOf(plugin)?.contributes.collections.find((c) => c.name === n),
    )
    return {
      ...base,
      live(query: Record<string, unknown> = {}) {
        const def = manifestOf(plugin)?.contributes.collections.find((c) => c.name === name)
        if (!def) throw new RookeryError("NOT_FOUND", `plugin ${plugin} declares no collection ${name}`)
        return createLive(s.rt, { plugin, collection: name, space: currentSpace() }, def, query as any)
      },
    }
  }
  return collectionsProxy((name) => collectionOf(s.plugin, name), {
    refs: createRefs(call),
    space: (id: string) => createData(s, id),
    collection: collectionOf,
    showable: () => {
      const all = s.rt.manifests?.() ?? [s.manifest]
      const out: Showable[] = []
      for (const m of all) {
        for (const def of m.contributes.collections) {
          if (!def.display) continue
          const scheme = def.entity?.scheme
          out.push({
            plugin: m.id,
            collection: def.name,
            display: def.display,
            def,
            ...(scheme ? { scheme } : {}),
            ...(scheme
              ? { entityView: m.contributes.entityTypes.find((t) => t.scheme === scheme)?.view }
              : {}),
          })
        }
      }
      return out
    },
  }) as unknown as Data
}

export function serviceUrl(plugin: string, service: string, path = "/"): string {
  const clean = path.startsWith("/") ? path : `/${path}`
  return `/svc/${encodeURIComponent(plugin)}/${encodeURIComponent(service)}${clean}`
}

/**
 * The event names a subscription of a plugin asks for: a bare name is the plugin's own, and one with
 * a `/` is a full name of the server, such as another plugin's or one of its prefixes.
 */
function eventNames(plugin: string, names: string | readonly string[]): string[] {
  return (typeof names === "string" ? [names] : [...names]).map((name) =>
    name.includes("/") ? name : `${plugin}/${name}`,
  )
}

/** The bare name `events.publish` takes: the server puts the plugin's name in front of it. */
function eventName(plugin: string, name: string): string {
  const own = `${plugin}/`
  return name.startsWith(own) ? name.slice(own.length) : name
}

export function createClientRookery(s: ClientScope): ClientRookery {
  const noop = () => disposable(() => {})
  /** A call of a method the server has, with its params and result from the generated types. */
  const call = <M extends WireMethod>(method: M, params: WireParams<M>): Promise<WireResult<M>> =>
    wireCall(s.rt, method, params)
  return Object.freeze<ClientRookery>({
    id: s.plugin,
    manifest: s.manifest,
    views: {
      register(id, component, options) {
        if (!s.manifest.contributes.views.some((v) => v.id === id)) {
          console.warn(`[${s.plugin}] view ${id} is not declared in plugin.json; the shell may not show it`)
        }
        return s.track(s.rt.registerView(s.plugin, id, component, options))
      },
      field(collection, field, parts) {
        const coll = s.manifest.contributes.collections.find((c) => c.name === collection)
        if (!coll?.fields.some((f) => f.name === field)) {
          throw new RookeryError(
            "INVALID",
            `${s.plugin}: ${field} is not a field of collection ${collection}`,
          )
        }
        if (!s.rt.registerFieldParts) {
          throw new RookeryError("UNAVAILABLE", "this shell does not draw declared views")
        }
        return s.track(s.rt.registerFieldParts(s.plugin, collection, field, parts))
      },
      section(view, id, component) {
        if (!s.rt.registerSection) {
          throw new RookeryError("UNAVAILABLE", "this shell does not draw record pages")
        }
        return s.track(s.rt.registerSection(s.plugin, view, id, component))
      },
      get View() {
        return s.rt.components?.View
      },
      href: (id, opts) => s.rt.href?.(id, opts) ?? null,
    },
    commands: {
      register: (id, def) => s.track(s.rt.registerCommand(s.plugin, id, def)),
      run: <T = unknown>(id: CommandId, params: Record<string, unknown> = {}) =>
        s.rt.runCommand(id, params) as Promise<T>,
      list: async () => ((await s.rt.call("commands.list", {})) as CommandList).commands,
    },
    nav: {
      section: (id, section) =>
        s.rt.registerNavSection ? s.track(s.rt.registerNavSection(s.plugin, id, section)) : noop(),
      badge: (entry, value) =>
        s.rt.registerNavBadge ? s.track(s.rt.registerNavBadge(s.plugin, entry, value)) : noop(),
    },
    chrome: {
      component: (id, component) =>
        s.rt.registerChromeComponent
          ? s.track(s.rt.registerChromeComponent(s.plugin, id, component))
          : noop(),
    },
    workbench: {
      openView: (id, opts) => s.rt.openView?.(id, opts),
      href: (id, opts) => s.rt.href?.(id, opts) ?? null,
      toast: (t) => s.rt.toast?.(t),
      statusItem: (id, component) =>
        s.rt.registerStatusItem ? s.track(s.rt.registerStatusItem(s.plugin, id, component)) : noop(),
    },
    events: {
      subscribe: async (names, handler, opts) =>
        s.track(await s.rt.subscribe(eventNames(s.plugin, names), handler, opts)),
      publish: (name, payload) =>
        call("events.publish", { plugin: s.plugin, name: eventName(s.plugin, name), payload }),
    },
    notifications: {
      on: <M extends NotificationMethod>(method: M, fn: (params: NotificationParams<M>) => void) =>
        s.rt.onNotification(method, (params) => fn(params as NotificationParams<M>)),
    },
    notify: {
      list: (query = {}) =>
        call("notify.list", {
          unread: query.unread ?? false,
          ...(query.before ? { before: query.before } : {}),
          ...(query.limit !== undefined ? { limit: query.limit } : {}),
        }),
      read: async (ids) => (await call("notify.read", ids ? { ids } : {})).count,
      answer: (id, answer) => call("notify.answer", { id, answer }),
      mute: async (kind, muted) => {
        await call("notify.setPref", { kind, muted })
      },
      send: (input) => call("notify.send", { plugin: s.plugin, ...input }),
    },
    data: createData(s),
    streams: {
      open: (method, params = {}, opts) => s.rt.stream(method, params, opts),
      channel: (channel, params, opts) =>
        s.rt.stream("streams.open", { plugin: s.plugin, channel, params }, opts),
    },
    host: {
      call: <T = unknown>(method: string, params: Record<string, unknown> = {}) =>
        s.rt.call("plugins.rpc", { plugin: s.plugin, method, params }) as Promise<T>,
      stream: (method, params = {}, opts) =>
        s.rt.stream("plugins.rpc", { plugin: s.plugin, method, params }, opts),
    },
    rpc: {
      call: <T = unknown>(method: string, params: Record<string, unknown> = {}) =>
        s.rt.call(method, params) as Promise<T>,
    },
    settings: {
      get: async <K extends SettingKey>(key: K) =>
        (await call("settings.get", { plugin: s.plugin, key })).value as TypedSettings[K],
      set: async (key, value, scope) => {
        await call("settings.set", { plugin: s.plugin, key, value, scope })
      },
      list: async () => (await call("settings.list", { plugin: s.plugin })).entries,
      reset: async (key, scope) => {
        await call("settings.reset", { plugin: s.plugin, key, scope })
      },
    },
    storage: {
      get: async <T = unknown>(key: string, opts: { scope?: Scope } = {}) =>
        (await call("storage.get", { plugin: s.plugin, key, scope: opts.scope ?? "global" }))
          .value as T | null,
      set: (key, value, opts = {}) =>
        call("storage.set", {
          plugin: s.plugin,
          key,
          value,
          scope: opts.scope ?? "global",
          ...(opts.revision !== undefined ? { revision: opts.revision } : {}),
        }),
      list: async (opts) =>
        (
          await call("storage.list", {
            plugin: s.plugin,
            prefix: opts.prefix,
            scope: opts.scope ?? "global",
            ...(opts.after ? { after: opts.after } : {}),
            ...(opts.limit !== undefined ? { limit: opts.limit } : {}),
          })
        ).entries,
      delete: async (key, opts = {}) =>
        (
          await call("storage.delete", {
            plugin: s.plugin,
            key,
            scope: opts.scope ?? "global",
            ...(opts.revision !== undefined ? { revision: opts.revision } : {}),
          })
        ).deleted,
    },
    jobs: {
      enqueue: (kind, params = {}, opts = {}) =>
        call("jobs.enqueue", {
          kind,
          params,
          ...(opts.subject ? { subject: opts.subject } : {}),
        }),
      get: (id) => call("jobs.get", { id }),
      list: async (query = {}) => (await call("jobs.list", query)).jobs,
      cancel: (id) => call("jobs.cancel", { id }),
      retry: (id) => call("jobs.retry", { id }),
      output: (id, opts = {}) => call("jobs.output", { id, after: opts.after ?? 0 }),
      follow: async (id, onItem, opts = {}) => {
        const stream = await s.rt.stream("jobs.follow", { id, after: opts.after ?? 0 })
        const dec = new TextDecoder()
        let buf = ""
        const line = (text: string) => {
          let parsed: unknown
          try {
            parsed = JSON.parse(text)
          } catch {
            return
          }
          // Each line is a `LogItem`; the last one is a `JobEnd`, which has the state and no seq.
          if (parsed && typeof parsed === "object" && "state" in parsed && !("seq" in parsed)) {
            opts.onDone?.(parsed as JobEnd)
          } else {
            onItem(parsed as LogItem)
          }
        }
        stream.onData = (chunk) => {
          buf += dec.decode(chunk, { stream: true })
          const lines = buf.split("\n")
          buf = lines.pop() ?? ""
          for (const l of lines) if (l.trim()) line(l)
        }
        stream.onEnd = () => {
          if (buf.trim()) line(buf)
        }
        return s.track(disposable(() => stream.close()))
      },
    },
    agents: {
      connect: (env, opts) => s.rt.stream("agents.connect", { env }, opts),
    },
    env: {
      list: async () => (await call("env.list", {})).environments,
      get: (env) => call("env.get", { env }),
      presence: (env) => call("env.presence", { env }),
      exec: (env, command, opts = {}) =>
        s.rt.call("env.exec", { env, command, ...opts }) as Promise<ExecResult>,
      readFile: (env, path) => call("env.readFile", { env, path }),
      writeFile: (env, path, content, opts = {}) =>
        call("env.writeFile", { env, path, base64: toBase64(content), ...opts }),
      deleteFile: async (env, path, opts = {}) => {
        await call("env.deleteFile", { env, path, ...opts })
      },
      listDir: (env, path) => call("env.listDir", { env, path }),
      stat: (env, path) => call("env.stat", { env, path }),
      connect: (env, port, opts) => s.rt.stream("env.connect", { env, port }, opts),
      portUrl: (env, port, path) => call("env.portUrl", { env, port, ...(path ? { path } : {}) }),
    },
    terminals: {
      open: (env, opts = {}) => call("terminals.open", { env, ...opts }),
      attach: (id, opts = {}, stream) => s.rt.stream("terminals.attach", { id, ...opts }, stream),
      write: async (id, text) => {
        await call("terminals.write", { id, text })
      },
      resize: async (id, cols, rows) => {
        await call("terminals.resize", { id, size: { cols, rows } })
      },
      signal: async (id, signal) => {
        await call("terminals.signal", { id, signal })
      },
      rename: async (id, title) => {
        await call("terminals.rename", { id, title })
      },
      close: async (id) => {
        await call("terminals.close", { id })
      },
      list: async (opts = {}) => (await call("terminals.list", opts)).terminals,
      get: (id) => call("terminals.get", { id }),
      screen: (id, opts = {}) => call("terminals.screen", { id, ...opts }),
      sessions: async (env, opts = {}) => (await call("terminals.sessions", { env, ...opts })).sessions,
      adopt: (env, session) => call("terminals.adopt", { env, session }),
    },
    connections: {
      types: async () => (await call("connections.types", {})).types,
      list: async (opts = {}) => (await call("connections.list", opts)).connections,
      get: (id) => call("connections.get", { id }),
      create: (input) => call("connections.create", input),
      update: (input) => call("connections.update", input),
      delete: async (id) => {
        await call("connections.delete", { id })
      },
      test: (id) => call("connections.test", { id }),
    },
    spaces: {
      current: () => s.rt.space?.() ?? null,
      list: async () => ((await s.rt.call("spaces.list", {})) as SpaceList).spaces,
      get: (id) => s.rt.call("spaces.get", { id }) as Promise<SpaceInfo>,
      create: (params) => s.rt.call("spaces.create", params) as Promise<SpaceInfo>,
      rename: (id, name) => s.rt.call("spaces.rename", { id, name }) as Promise<SpaceInfo>,
      enter: async (id) => {
        await s.rt.call("spaces.enter", { id })
      },
    },
    contexts: {
      provide: (id, provider) => (s.rt.provideContext ? s.track(s.rt.provideContext(id, provider)) : noop()),
      value: (id) => s.rt.contextValue?.(id),
      switch: (id, value) => s.rt.switchContext?.(id, value),
    },
    blobs: {
      get: (id) => call("blobs.get", { id }),
      url: (id) => `/blobs/${encodeURIComponent(id)}`,
    },
    extensions: {
      list: async (point) => ((await s.rt.call("extensions.list", { point })) as ExtensionList).contributions,
    },
    plugins: {
      call: <T = unknown>(plugin: string, method: string, params: Record<string, unknown> = {}) =>
        s.rt.call("plugins.rpc", { plugin, method, params }) as Promise<T>,
      stream: (plugin, method, params = {}, opts) =>
        s.rt.stream("plugins.rpc", { plugin, method, params }, opts),
    },
    appServices: {
      url: (service, path) => serviceUrl(s.plugin, service, path),
      list: async () => ((await s.rt.call("services.list", { plugin: s.plugin })) as AppServiceList).services,
      status: (service) =>
        s.rt.call("services.status", { plugin: s.plugin, service }) as Promise<AppServiceStatus>,
    },
  })
}
