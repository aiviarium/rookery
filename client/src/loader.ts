// Plugin client-half loader and registry. Tracks plugins from the server (`plugins.list`,
// `plugins.changed`, `plugins.reloaded`), dynamically imports content-hashed client bundles, manages
// stylesheets and activation scopes, and exposes reactive registrations to any shell.
import * as sdkClient from "@rookery/sdk/client"
import {
  type AdminPageEntry,
  type ChromeComponentProps,
  type ClientCommandDef,
  type ClientRuntime,
  createClientApi,
  isClientPlugin,
  type NavSection,
  type ViewComponent,
  type ViewOptions,
} from "@rookery/sdk/runtime/client"
import { disposable } from "@rookery/sdk/types"
import * as solid from "solid-js"
import { type Accessor, type Component, createSignal, type JSX } from "solid-js"
import * as solidStore from "solid-js/store"
import { createStore, reconcile } from "solid-js/store"
import * as solidWeb from "solid-js/web"
import { createEventBus, type EventBus } from "./events"
import type {
  CommandInfo,
  CommandList,
  PluginChanged,
  PluginInfo,
  PluginList,
  PluginReloaded,
} from "./generated"
import type { Connection } from "./ws"

export const SHARED_MODULE_GLOBAL = "__rookery"

/**
 * Publishes shared framework modules on `globalThis.__rookery.modules` so bundled plugin client halves
 * share one SolidJS runtime and SDK copy with the host page. A shell may pass extra modules (such as
 * its UI component library).
 */
export function publishSharedModules(extra: Record<string, unknown> = {}) {
  ;(globalThis as any)[SHARED_MODULE_GLOBAL] = {
    modules: {
      "solid-js": solid,
      "solid-js/web": solidWeb,
      "solid-js/store": solidStore,
      "@rookery/sdk/client": sdkClient,
      ...extra,
    },
  }
}

export interface ViewEntry {
  plugin: string
  component: ViewComponent
  /** Incremented on every registration so a reloaded plugin's views remount. */
  version: number
  followsPath: boolean
}

export interface ClientState {
  status: "loading" | "ready" | "failed"
  hash?: string
  error?: string
}

export interface PluginLoaderOptions {
  /** Event bus for plugin event subscriptions (created on `conn` if omitted). */
  eventBus?: EventBus
  /** Optional shell-provided runtime callbacks (`openView`, `href`, `toast`, `space`, `me`, `components`, ...). */
  runtime?: Partial<ClientRuntime>
  /** Custom module importer (defaults to dynamic `import()`). */
  importModule?: (url: string) => Promise<{ default?: unknown }>
  /** Optional base URL prepended to relative `client.script` / `client.styles` URLs. */
  baseUrl?: string
  /** Activation timeout per plugin client half in ms. Default 5000. */
  activationTimeoutMs?: number
  /** Document to attach plugin `<link rel="stylesheet">` elements to (defaults to `globalThis.document`). */
  document?: Document
}

export interface PluginLoader {
  readonly plugins: Readonly<Record<string, PluginInfo>>
  readonly clients: Readonly<Record<string, ClientState>>
  readonly views: Accessor<Record<string, ViewEntry>>
  readonly clientCommands: Accessor<Record<string, { plugin: string; def: ClientCommandDef }>>
  readonly commands: Accessor<CommandInfo[]>
  readonly statusItems: Accessor<Record<string, { plugin: string; component: Component }>>
  readonly navSections: Accessor<Record<string, { plugin: string; id: string; section: NavSection }>>
  readonly navBadges: Accessor<Record<string, () => string | number | undefined>>
  readonly chromeComponents: Accessor<
    Record<string, { plugin: string; id: string; component: (props: ChromeComponentProps) => JSX.Element }>
  >
  readonly loaded: Accessor<boolean>
  readonly runtime: ClientRuntime
  start(): void
  stop(): void
  refresh(): Promise<void>
  clientsSettled(timeoutMs?: number): Promise<void>
  reloads(plugin: string): number
  declaredView(
    id: string,
  ):
    | { plugin: PluginInfo; view: NonNullable<PluginInfo["manifest"]>["contributes"]["views"][number] }
    | undefined
  adminPages(): AdminPageEntry[]
  adminPageOfView(viewId: string): AdminPageEntry | undefined
  activatePlugin(id: string): void
  runCommand(id: string, params?: Record<string, unknown>): Promise<unknown>
  pluginApi(plugin: string): ReturnType<typeof createClientApi>
}

function resolveAssetUrl(baseUrl: string | undefined, path: string): string {
  if (!baseUrl || /^https?:\/\/|^data:|^blob:/.test(path)) return path
  const base = baseUrl.replace(/\/+$/, "")
  const rel = path.startsWith("/") ? path : `/${path}`
  return `${base}${rel}`
}

export function createPluginLoader(conn: Connection, options: PluginLoaderOptions = {}): PluginLoader {
  const ownsBus = !options.eventBus
  const bus = options.eventBus ?? createEventBus(conn)
  const importModule = options.importModule ?? ((url: string) => import(/* @vite-ignore */ url))
  const activationTimeoutMs = options.activationTimeoutMs ?? 5000

  const [plugins, setPlugins] = createStore<Record<string, PluginInfo>>({})
  const [clients, setClients] = createStore<Record<string, ClientState>>({})
  const [views, setViews] = createSignal<Record<string, ViewEntry>>({})
  const [clientCommands, setClientCommands] = createSignal<
    Record<string, { plugin: string; def: ClientCommandDef }>
  >({})
  const [statusItems, setStatusItems] = createSignal<
    Record<string, { plugin: string; component: Component }>
  >({})
  const [navSections, setNavSections] = createSignal<
    Record<string, { plugin: string; id: string; section: NavSection }>
  >({})
  const [navBadges, setNavBadges] = createSignal<Record<string, () => string | number | undefined>>({})
  const [chromeComponents, setChromeComponents] = createSignal<
    Record<string, { plugin: string; id: string; component: (props: ChromeComponentProps) => JSX.Element }>
  >({})
  const [commands, setCommands] = createSignal<CommandInfo[]>([])
  const [loaded, setLoaded] = createSignal(false)
  const [reloadCounts, setReloadCounts] = createStore<Record<string, number>>({})

  let version = 0
  const disposers = new Map<string, () => void>()
  const styles = new Map<string, HTMLLinkElement>()
  const loading = new Set<string>()
  const settleWaiters: Array<() => void> = []
  let unsubs: Array<() => void> = []
  let started = false

  const settleCheck = () => {
    if (!loading.size && loaded()) {
      for (const done of settleWaiters.splice(0)) done()
    }
  }

  const clientsSettled = (timeoutMs = 5000): Promise<void> => {
    if (!loading.size && loaded()) return Promise.resolve()
    return new Promise((done) => {
      const t = setTimeout(done, timeoutMs)
      settleWaiters.push(() => {
        clearTimeout(t)
        done()
      })
    })
  }

  const registerView = (plugin: string, id: string, component: ViewComponent, opts: ViewOptions = {}) => {
    const entry: ViewEntry = { plugin, component, version: ++version, followsPath: !!opts.followsPath }
    setViews((v) => ({ ...v, [id]: entry }))
    return disposable(() =>
      setViews((v) => {
        if (v[id] !== entry) return v
        const { [id]: _, ...rest } = v
        return rest
      }),
    )
  }

  const registerCommand = (plugin: string, id: string, def: ClientCommandDef) => {
    const entry = { plugin, def }
    setClientCommands((c) => ({ ...c, [id]: entry }))
    return disposable(() =>
      setClientCommands((c) => {
        if (c[id] !== entry) return c
        const { [id]: _, ...rest } = c
        return rest
      }),
    )
  }

  const registerStatusItem = (plugin: string, id: string, component: Component) => {
    const key = `${plugin}:${id}`
    const entry = { plugin, component }
    setStatusItems((c) => ({ ...c, [key]: entry }))
    return disposable(() =>
      setStatusItems((c) => {
        if (c[key] !== entry) return c
        const { [key]: _, ...rest } = c
        return rest
      }),
    )
  }

  const registerNavSection = (plugin: string, id: string, section: NavSection) => {
    const key = `${plugin}:${id}`
    const entry = { plugin, id, section }
    setNavSections((c) => ({ ...c, [key]: entry }))
    return disposable(() =>
      setNavSections((c) => {
        if (c[key] !== entry) return c
        const { [key]: _, ...rest } = c
        return rest
      }),
    )
  }

  const registerNavBadge = (_plugin: string, entryId: string, value: () => string | number | undefined) => {
    setNavBadges((c) => ({ ...c, [entryId]: value }))
    return disposable(() =>
      setNavBadges((c) => {
        if (c[entryId] !== value) return c
        const { [entryId]: _, ...rest } = c
        return rest
      }),
    )
  }

  const registerChromeComponent = (
    plugin: string,
    id: string,
    component: (props: ChromeComponentProps) => JSX.Element,
  ) => {
    const key = `${plugin}:${id}`
    const entry = { plugin, id, component }
    setChromeComponents((c) => ({ ...c, [key]: entry }))
    return disposable(() =>
      setChromeComponents((c) => {
        if (c[key] !== entry) return c
        const { [key]: _, ...rest } = c
        return rest
      }),
    )
  }

  const runCommand = async (id: string, params: Record<string, unknown> = {}) => {
    let local = clientCommands()[id]
    if (!local) {
      await clientsSettled()
      local = clientCommands()[id]
    }
    if (local) return local.def.run(params)
    return conn.call("commands.run", { id, params })
  }

  const runtime: ClientRuntime = {
    call: (method, params) => conn.call(method, params),
    stream: (method, params, opts) => conn.stream(method, params, opts),
    onNotification: (method, fn) => disposable(conn.on(method, fn)),
    onReconnect: (fn) => disposable(conn.onReconnect(fn)),
    subscribe: (topic, handler) => bus.subscribe(topic, handler),
    registerView,
    registerCommand,
    registerStatusItem,
    registerNavSection,
    registerNavBadge,
    registerChromeComponent,
    runCommand,
    manifests: () => Object.values(plugins).flatMap((p) => (p?.manifest ? [p.manifest] : [])),
    ...options.runtime,
  }

  const unload = (id: string) => {
    disposers.get(id)?.()
    disposers.delete(id)
    styles.get(id)?.remove()
    styles.delete(id)
  }

  const getDoc = (): Document | undefined =>
    options.document ?? (typeof document !== "undefined" ? document : undefined)

  const loadClient = async (info: PluginInfo) => {
    const client = info.client
    if (!client || !info.manifest) {
      if (!client) unload(info.id)
      if (info.manifest?.client && !client) {
        setClients(info.id, { status: info.error ? "failed" : "loading", error: info.error?.message })
      }
      return
    }
    if (clients[info.id]?.hash === client.hash && disposers.has(info.id)) return
    if (
      clients[info.id]?.hash === client.hash &&
      clients[info.id]?.status === "loading" &&
      loading.has(info.id)
    ) {
      return
    }
    loading.add(info.id)
    setClients(info.id, { status: "loading", hash: client.hash })
    try {
      const scriptUrl = resolveAssetUrl(options.baseUrl, client.script)
      const mod = await importModule(scriptUrl)
      if (!isClientPlugin(mod.default)) {
        throw new Error("the client entry must `export default defineClient(...)`")
      }
      unload(info.id)
      const doc = getDoc()
      if (client.styles && doc?.head) {
        const link = doc.createElement("link")
        link.rel = "stylesheet"
        link.href = resolveAssetUrl(options.baseUrl, client.styles)
        link.dataset.plugin = info.id
        doc.head.append(link)
        styles.set(info.id, link)
      }
      const api = createClientApi(runtime, info.id, info.manifest)
      disposers.set(info.id, api.dispose)
      let timeout: ReturnType<typeof setTimeout> | undefined
      try {
        await Promise.race([
          Promise.resolve(mod.default.activate(api.rookery)),
          new Promise((_, reject) => {
            timeout = setTimeout(
              () => reject(new Error(`activation took longer than ${activationTimeoutMs} ms`)),
              activationTimeoutMs,
            )
          }),
        ])
      } finally {
        clearTimeout(timeout)
      }
      setClients(info.id, { status: "ready", hash: client.hash })
    } catch (e) {
      unload(info.id)
      setClients(info.id, {
        status: "failed",
        hash: client.hash,
        error: e instanceof Error ? e.message : String(e),
      })
    } finally {
      loading.delete(info.id)
      settleCheck()
    }
  }

  const refreshCommands = async () => {
    try {
      setCommands((await conn.call<CommandList>("commands.list")).commands)
    } catch {}
  }

  const refresh = async () => {
    const { plugins: list } = await conn.call<PluginList>("plugins.list")
    setPlugins(reconcile(Object.fromEntries(list.map((p) => [p.id, p]))))
    for (const id of [...disposers.keys()]) {
      if (!list.some((p) => p.id === id)) unload(id)
    }
    await Promise.all(list.map(loadClient))
    await refreshCommands()
    setLoaded(true)
    settleCheck()
  }

  const start = () => {
    if (started) return
    started = true
    unsubs = [
      conn.on("plugins.changed", (p: PluginChanged) => {
        if (p.plugin) {
          setPlugins(p.id, reconcile(p.plugin))
          void loadClient(p.plugin)
        } else {
          setPlugins(p.id, undefined!)
          unload(p.id)
        }
        void refreshCommands()
      }),
      conn.on("plugins.reloaded", async (r: PluginReloaded) => {
        setReloadCounts(r.id, (n) => (n ?? 0) + 1)
        const { plugins: list } = await conn.call<PluginList>("plugins.list")
        const info = list.find((p) => p.id === r.id)
        if (!info) return
        setPlugins(r.id, reconcile(info))
        await loadClient(info)
        await refreshCommands()
      }),
      conn.onOpen(() => {
        void refresh().catch(() => {})
      }),
    ]
  }

  const stop = () => {
    if (!started) return
    started = false
    for (const off of unsubs.splice(0)) off()
    for (const id of [...disposers.keys()]) unload(id)
    if (ownsBus) bus.dispose()
  }

  const declaredView = (id: string) => {
    for (const p of Object.values(plugins)) {
      const v = p?.manifest?.contributes.views.find((x) => x.id === id)
      if (v) return { plugin: p, view: v }
    }
    return undefined
  }

  const adminPages = (): AdminPageEntry[] =>
    Object.values(plugins).flatMap((p) =>
      p && p.state !== "invalid"
        ? (p.manifest?.contributes.adminPages ?? []).map((page) => ({ plugin: p, page }))
        : [],
    )

  return {
    plugins,
    clients,
    views,
    clientCommands,
    commands,
    statusItems,
    navSections,
    navBadges,
    chromeComponents,
    loaded,
    runtime,
    start,
    stop,
    refresh,
    clientsSettled,
    reloads: (plugin: string) => reloadCounts[plugin] ?? 0,
    declaredView,
    adminPages,
    adminPageOfView: (viewId: string) => adminPages().find((e) => e.page.view === viewId),
    activatePlugin: (id: string) => {
      const p = plugins[id]
      if (p?.manifest?.host && p.state === "inactive") {
        conn.call("plugins.activate", { id }).catch(() => {})
      }
    },
    runCommand,
    pluginApi: (plugin: string) => {
      const manifest = plugins[plugin]?.manifest
      if (!manifest) throw new Error(`plugin ${plugin} is not loaded`)
      return createClientApi(runtime, plugin, manifest)
    },
  }
}
