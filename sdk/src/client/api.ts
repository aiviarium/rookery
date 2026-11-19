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
  ConnectionList,
  ConnectionTypeInfo,
  ConnectionTypeList,
  ConnectionUpdate,
  DirListing,
  EnvInfo,
  EnvList,
  EnvPresence,
  ExecResult,
  ExtensionContribution,
  ExtensionList,
  FileData,
  FileWritten,
  JobDetail,
  JobInfo,
  JobList,
  JobLogItem,
  JobOutput,
  Manifest,
  ScreenInfo,
  SettingEntry,
  SettingScope,
  SettingsView,
  SettingValue,
  SpaceInfo,
  SpaceList,
  StorageEntries,
  StorageScope,
  StorageValue,
  TerminalInfo,
  TerminalList,
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
import { createLive, type Live } from "./live"
import type {
  ChromeComponentProps,
  ClientCommandDef,
  ClientScope,
  ClientStream,
  ContextProvider,
  EmbeddedViewProps,
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
  subscribe(topic: string, handler: (payload: unknown, topic: string) => unknown): Promise<Disposable>
  publish(topic: string, payload?: unknown): Promise<void>
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
  get<K extends SettingKey>(key: K, opts?: { space?: string }): Promise<TypedSettings[K]>
  set<K extends SettingKey>(
    key: K,
    value: TypedSettings[K],
    opts?: { scope?: SettingScope; space?: string },
  ): Promise<void>
  list(opts?: { space?: string }): Promise<SettingEntry[]>
  reset(key: SettingKey, opts?: { scope?: SettingScope; space?: string }): Promise<void>
}

export interface Storage {
  get<T = unknown>(key: string, opts?: { scope?: StorageScope; space?: string }): Promise<T | null>
  set(
    key: string,
    value: unknown,
    opts?: { scope?: StorageScope; space?: string; expectedRevision?: number },
  ): Promise<void>
  list(opts?: { scope?: StorageScope; space?: string; prefix?: string }): Promise<StorageEntries["entries"]>
  delete(key: string, opts?: { scope?: StorageScope; space?: string }): Promise<void>
}

export interface JobFollower {
  dispose(): void
}

export interface Jobs {
  enqueue(job: string, params?: Record<string, unknown>): Promise<JobInfo>
  get(id: string): Promise<JobDetail>
  list(query?: { state?: string; plugin?: string; limit?: number }): Promise<JobInfo[]>
  cancel(id: string): Promise<void>
  retry(id: string): Promise<JobInfo>
  output(id: string, opts?: { after?: number; limit?: number }): Promise<JobOutput>
  follow(
    id: string,
    onItem: (item: JobLogItem) => void,
    opts?: { after?: number; onDone?: (state: string) => void },
  ): Promise<JobFollower>
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
  writeFile(env: string, path: string, content: string, opts?: { rev?: string }): Promise<FileWritten>
  deleteFile(env: string, path: string): Promise<void>
  listDir(env: string, path: string): Promise<DirListing>
  connect(env: string, port: number, opts?: StreamOptions): Promise<ClientStream>
}

export interface ClientTerminals {
  open(
    env: string,
    opts?: { cols?: number; rows?: number; command?: string[]; cwd?: string },
  ): Promise<ClientStream>
  attach(terminal: string, opts?: { cols?: number; rows?: number }): Promise<ClientStream>
  write(terminal: string, data: string): Promise<void>
  resize(terminal: string, cols: number, rows: number): Promise<void>
  signal(terminal: string, signal: string): Promise<void>
  close(terminal: string): Promise<void>
  list(env?: string): Promise<TerminalInfo[]>
  get(terminal: string): Promise<TerminalInfo>
  screen(terminal: string): Promise<ScreenInfo>
}

export interface Connections {
  types(): Promise<ConnectionTypeInfo[]>
  list(opts?: { type?: string; space?: string }): Promise<ConnectionInfo[]>
  get(id: string): Promise<ConnectionInfo>
  create(input: ConnectionCreate): Promise<ConnectionInfo>
  update(input: ConnectionUpdate): Promise<ConnectionInfo>
  delete(id: string): Promise<void>
  test(id: string): Promise<{ ok: boolean; error?: string }>
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
  readonly data: Data
  readonly streams: Streams
  readonly host: Host
  readonly rpc: Rpc
  readonly settings: Settings
  readonly storage: Storage
  readonly jobs: Jobs
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

export function createClientRookery(s: ClientScope): ClientRookery {
  const noop = () => disposable(() => {})
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
      subscribe: async (topic, handler) => s.track(await s.rt.subscribe(topic, handler)),
      publish: async (topic, payload) => {
        await s.rt.call("events.publish", { topic, payload })
      },
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
      get: async (key, opts) =>
        ((await s.rt.call("settings.get", { plugin: s.plugin, key, ...opts })) as SettingValue).value as any,
      set: async (key, value, opts) => {
        await s.rt.call("settings.set", { plugin: s.plugin, key, value, ...opts })
      },
      list: async (opts) =>
        ((await s.rt.call("settings.list", { plugin: s.plugin, ...opts })) as SettingsView).entries,
      reset: async (key, opts) => {
        await s.rt.call("settings.reset", { plugin: s.plugin, key, ...opts })
      },
    },
    storage: {
      get: async (key, opts) =>
        ((await s.rt.call("storage.get", { plugin: s.plugin, key, ...opts })) as StorageValue).value as any,
      set: async (key, value, opts) => {
        await s.rt.call("storage.set", { plugin: s.plugin, key, value, ...opts })
      },
      list: async (opts) =>
        ((await s.rt.call("storage.list", { plugin: s.plugin, ...opts })) as StorageEntries).entries,
      delete: async (key, opts) => {
        await s.rt.call("storage.delete", { plugin: s.plugin, key, ...opts })
      },
    },
    jobs: {
      enqueue: (job, params = {}) =>
        s.rt.call("jobs.enqueue", { plugin: s.plugin, job, params }) as Promise<JobInfo>,
      get: (id) => s.rt.call("jobs.get", { id }) as Promise<JobDetail>,
      list: async (query = {}) => ((await s.rt.call("jobs.list", query)) as JobList).jobs,
      cancel: async (id) => {
        await s.rt.call("jobs.cancel", { id })
      },
      retry: (id) => s.rt.call("jobs.retry", { id }) as Promise<JobInfo>,
      output: (id, opts = {}) => s.rt.call("jobs.output", { id, ...opts }) as Promise<JobOutput>,
      follow: async (id, onItem, opts = {}) => {
        const stream = await s.rt.stream("jobs.follow", { id, after: opts.after ?? 0 })
        const dec = new TextDecoder()
        let buf = ""
        stream.onData = (chunk) => {
          buf += dec.decode(chunk, { stream: true })
          const lines = buf.split("\n")
          buf = lines.pop() ?? ""
          for (const line of lines) {
            if (!line.trim()) continue
            try {
              const parsed = JSON.parse(line)
              if (parsed && typeof parsed === "object" && "state" in parsed && !("seq" in parsed)) {
                opts.onDone?.(String(parsed.state))
              } else {
                onItem(parsed as JobLogItem)
              }
            } catch {}
          }
        }
        stream.onEnd = () => {
          if (buf.trim()) {
            try {
              onItem(JSON.parse(buf) as JobLogItem)
            } catch {}
          }
        }
        return s.track(disposable(() => stream.close()))
      },
    },
    env: {
      list: async () => ((await s.rt.call("env.list", {})) as EnvList).environments,
      get: (env) => s.rt.call("env.get", { env }) as Promise<EnvInfo>,
      presence: (env) => s.rt.call("env.presence", { env }) as Promise<EnvPresence>,
      exec: (env, command, opts = {}) =>
        s.rt.call("env.exec", { env, command, ...opts }) as Promise<ExecResult>,
      readFile: (env, path) => s.rt.call("env.readFile", { env, path }) as Promise<FileData>,
      writeFile: (env, path, content, opts = {}) =>
        s.rt.call("env.writeFile", { env, path, content, ...opts }) as Promise<FileWritten>,
      deleteFile: async (env, path) => {
        await s.rt.call("env.deleteFile", { env, path })
      },
      listDir: (env, path) => s.rt.call("env.listDir", { env, path }) as Promise<DirListing>,
      connect: (env, port, opts) => s.rt.stream("env.connect", { env, port }, opts),
    },
    terminals: {
      open: (env, opts = {}) => s.rt.stream("terminals.open", { env, ...opts }),
      attach: (terminal, opts = {}) => s.rt.stream("terminals.attach", { terminal, ...opts }),
      write: async (terminal, data) => {
        await s.rt.call("terminals.write", { terminal, data })
      },
      resize: async (terminal, cols, rows) => {
        await s.rt.call("terminals.resize", { terminal, cols, rows })
      },
      signal: async (terminal, signal) => {
        await s.rt.call("terminals.signal", { terminal, signal })
      },
      close: async (terminal) => {
        await s.rt.call("terminals.close", { terminal })
      },
      list: async (env) =>
        ((await s.rt.call("terminals.list", env ? { env } : {})) as TerminalList).terminals,
      get: (terminal) => s.rt.call("terminals.get", { terminal }) as Promise<TerminalInfo>,
      screen: (terminal) => s.rt.call("terminals.screen", { terminal }) as Promise<ScreenInfo>,
    },
    connections: {
      types: async () => ((await s.rt.call("connections.types", {})) as ConnectionTypeList).types,
      list: async (opts = {}) => ((await s.rt.call("connections.list", opts)) as ConnectionList).connections,
      get: (id) => s.rt.call("connections.get", { id }) as Promise<ConnectionInfo>,
      create: (input) =>
        s.rt.call(
          "connections.create",
          input as unknown as Record<string, unknown>,
        ) as Promise<ConnectionInfo>,
      update: (input) =>
        s.rt.call(
          "connections.update",
          input as unknown as Record<string, unknown>,
        ) as Promise<ConnectionInfo>,
      delete: async (id) => {
        await s.rt.call("connections.delete", { id })
      },
      test: (id) => s.rt.call("connections.test", { id }) as Promise<{ ok: boolean; error?: string }>,
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
      get: (id) => s.rt.call("blobs.get", { id }) as Promise<BlobInfo>,
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
