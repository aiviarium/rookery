// The client half's runtime contract. A shell implements ClientRuntime and creates one `rookery` handle per
// plugin with createClientApi; everything the plugin registers is disposed when the plugin reloads or unloads.
import type { Component, JSX } from "solid-js"
import type {
  AdminPageContribution,
  BlockWidth,
  EntityInfo,
  FieldContribution,
  Manifest,
  PluginInfo,
  SpaceInfo,
  WidgetArea,
  WidgetContribution,
} from "../generated"
import type { Disposable } from "../types"
import { createClientRookery } from "./api"

/** What a view component receives. */
export interface ViewProps {
  view: ViewHandle
}

export interface ViewHandle {
  /** The space of the view's URL, for a view whose route is mounted under the space context. */
  readonly space?: SpaceInfo | null
  readonly id: string
  readonly plugin: string
  /**
   * The values of the route's parameters and its rest, joined with `/`. Fixed for the life of the view,
   * unless it was registered with `followsPath`: then it changes as the view navigates within itself.
   */
  readonly path: string
  /** The route's parameters by name, and the URL's query. Follows the path like `path`. */
  readonly params: Readonly<Record<string, string>>
  /** The values of the contexts the view's route is mounted under, by context id. */
  readonly context?: Readonly<Record<string, string>>
  setTitle(title: string): void
  setDirty(dirty: boolean): void
  setLoading(loading: boolean): void
}

export type ViewComponent = (props: ViewProps) => JSX.Element

/** How a shell mounts a registered view. */
export interface ViewOptions {
  /** Keeps the view mounted while its tab moves to another path of the same view. */
  followsPath?: boolean
}

export interface FieldPartProps {
  plugin: string
  collection: string
  field: FieldContribution
  value: unknown
  record: Record<string, unknown>
}

export interface FieldEditorProps extends FieldPartProps {
  onCommit(value: unknown): void
  error?: string
  disabled: boolean
}

export interface FieldParts {
  cell?: (props: FieldPartProps) => JSX.Element
  value?: (props: FieldPartProps) => JSX.Element
  editor?: (props: FieldEditorProps) => JSX.Element
}

export interface RecordSectionProps {
  plugin: string
  collection: string
  record: Record<string, unknown> & { id: string }
  uri?: string
}

export interface ClientCommandDef {
  title?: string
  run(params: Record<string, unknown>): unknown
}

export interface Toast {
  title: string
  body?: string
  kind?: "info" | "success" | "warning" | "danger"
}

export interface OpenViewOptions {
  path?: string
  params?: Record<string, string>
  context?: Record<string, string>
  split?: boolean
  replace?: boolean
}

export interface NavItem {
  id: string
  title: string
  icon?: string
  view?: string
  path?: string
  context?: Record<string, string>
  href?: string
  onSelect?: () => void
  badge?: string | number
  current?: boolean
  children?: NavItem[]
  group?: string
}

export interface NavSectionProps {
  rail: boolean
  onNavigate: () => void
}

export interface NavSection {
  items?: () => NavItem[]
  component?: Component<NavSectionProps>
  badge?: () => string | number | undefined
  empty?: () => boolean
}

export interface ChromeComponentProps {
  area: string
}

export interface ContextValueInfo {
  value: string
  title: string
  icon?: string
  subtitle?: string
}

export interface ContextProvider {
  list(): ContextValueInfo[]
  resolve?(value: string): ContextValueInfo | undefined
  default?(): string | undefined
}

export interface EmbeddedViewProps {
  id: string
  path?: string
  params?: Record<string, string>
  class?: string
}

export interface DesktopViewProps {
  env: string
  viewOnly?: boolean
  scale?: boolean
  class?: string
  onStateChange?: (state: "connecting" | "connected" | "disconnected" | "error", detail?: string) => void
}

/** What a plugin's block component receives in the document editor (`rookery.blocks.register`). */
export interface BlockProps<A extends Record<string, unknown> = Record<string, unknown>> {
  /** The block's attributes (reactive: remote edits update them). */
  readonly attrs: A
  /** Merges a change into the attributes; it syncs like any edit. Ignored when read-only. */
  setAttrs(patch: Partial<A>): void
  /** The document is read-only, or the block is shown in a preview. */
  readonly readOnly: boolean
  /** The block is selected in the editor. */
  readonly selected: boolean
  /** The headings of the document the block is in (reactive), for a table of contents. */
  outline(): Array<{ level: number; text: string; pos: number }>
  /** Scrolls the editor to a position (a heading's `pos`) and puts the cursor there. */
  scrollTo(pos: number): void
}

export type BlockComponent = (props: BlockProps<any>) => JSX.Element

/** How a block's component is registered (`rookery.blocks.register(id, component, options)`). */
export interface BlockOptions {
  /** Forces a width from the block's attributes over the manifest's `width` policy. */
  forceWidth?: (attrs: Record<string, unknown>) => BlockWidth | null | undefined
}

/** A widget placed on a grid: which widget, where, how big, and its settings. */
export interface WidgetInstance {
  id: string
  type: string
  x: number
  y: number
  w: number
  h: number
  settings?: Record<string, unknown>
}

/** A widget a plugin (or the shell) declares. */
export type WidgetType = WidgetContribution & { plugin: string }

/** What a widget's component receives. */
export interface WidgetHandle {
  readonly type: string
  readonly plugin: string
  readonly instance: string
  readonly area: WidgetArea
  readonly settings: Readonly<Record<string, unknown>>
  readonly size: Readonly<{ w: number; h: number }>
  readonly editing: boolean
  refreshed(): number
  update(patch: Record<string, unknown>): void
}

export interface WidgetProps {
  widget: WidgetHandle
}

export type WidgetComponent = (props: WidgetProps) => JSX.Element

export interface WidgetAreaProps {
  instances: WidgetInstance[]
  onChange(next: WidgetInstance[]): void
  editing: boolean
  area: WidgetArea
  label?: string
}

/** A chip for any entity, rendered from the entity index: title and icon, a link that opens it. */
export interface EntityChipProps {
  uri: string
  label?: string
  plain?: boolean
}

export type EntityDensity = "compact" | "comfortable"

/** What `<rookery.entities.Card>` and `<rookery.entities.Row>` take. */
export interface EntityViewProps {
  uri: string
  record?: Record<string, unknown>
  density?: EntityDensity
  groupedBy?: string
  selected?: boolean
  onOpen?: () => void
  fallback?: JSX.Element
}

/** What a card or row renderer of the plugin that owns a scheme receives. */
export interface EntityRendererProps {
  uri: string
  scheme: string
  id: string
  record?: Record<string, unknown>
  density: EntityDensity
  groupedBy?: string
  selected?: boolean
  onOpen: () => void
}

export type EntityRenderer = (props: EntityRendererProps) => JSX.Element

/** A popup listing entities that match `query`, for `@` mentions. */
export interface MentionPickerProps {
  query: string
  anchor: HTMLElement | undefined
  open: boolean
  onPick: (entity: EntityInfo) => void
  onClose: () => void
  schemes?: string[]
}

export interface ShellComponents {
  View?: Component<EmbeddedViewProps>
  DesktopView?: Component<DesktopViewProps>
  EntityChip?: Component<EntityChipProps>
  EntityCard?: Component<EntityViewProps>
  EntityRow?: Component<EntityViewProps>
  MentionPicker?: Component<MentionPickerProps>
  WidgetArea?: Component<WidgetAreaProps>
}

/** A stream as the client framework gives it to a client half. */
export interface ClientStream {
  readonly id: number
  /** The result of the method that opened it. */
  readonly result: any
  write(data: Uint8Array): Promise<void>
  eof(): void
  close(): void
  /** Set by the owner: data as it arrives. */
  onData: (data: Uint8Array) => void
  /** Set by the owner: the stream ended, optionally with an error. */
  onEnd: (error?: { code: string; message: string; details?: unknown }) => void
  /** Grants the server credit for `bytes` it sent, for streams opened with `flow: "manual"`. */
  grant?(bytes: number): void
}

export interface StreamOptions {
  /** Leaves granting credit to the stream's owner (`ClientStream.grant`). */
  flow?: "manual"
}

export interface AdminPageEntry {
  plugin: PluginInfo
  page: AdminPageContribution
}

/** What a subscriber does with an event: `<plugin>/<name>` is the name the server sent it under. */
export type EventHandler = (payload: unknown, name: string) => unknown

export interface EventSubscriptionOptions {
  /**
   * Events of this subscription that did not fit on the connection were dropped, and are lost. The
   * subscriber should read again what it shows, or subscribe again if it needs what it missed.
   */
  onDropped?: (dropped: number) => void
}

export interface ClientRuntime {
  call(method: string, params: Record<string, unknown>): Promise<unknown>
  stream(method: string, params: Record<string, unknown>, opts?: StreamOptions): Promise<ClientStream>
  onNotification(method: string, fn: (params: any) => void): Disposable
  onReconnect(fn: () => void): Disposable
  /**
   * Subscribes to the events named by `names`, each `<plugin>/<name>` or a prefix ending in `*`, as
   * `events.event` notifications. The returned disposable ends the subscription.
   */
  subscribe(
    names: string | readonly string[],
    handler: EventHandler,
    opts?: EventSubscriptionOptions,
  ): Promise<Disposable>
  registerView(plugin: string, id: string, component: ViewComponent, options?: ViewOptions): Disposable
  registerCommand(plugin: string, id: string, def: ClientCommandDef): Disposable
  registerStatusItem?(plugin: string, id: string, component: Component): Disposable
  registerNavSection?(plugin: string, id: string, section: NavSection): Disposable
  registerNavBadge?(plugin: string, entry: string, value: () => string | number | undefined): Disposable
  registerChromeComponent?(
    plugin: string,
    id: string,
    component: (props: ChromeComponentProps) => JSX.Element,
  ): Disposable
  registerFieldParts?(plugin: string, collection: string, field: string, parts: FieldParts): Disposable
  registerSection?(
    plugin: string,
    view: string,
    id: string,
    component: (props: RecordSectionProps) => JSX.Element,
  ): Disposable
  registerBlock?(plugin: string, id: string, component: BlockComponent, options?: BlockOptions): Disposable
  registerWidget?(plugin: string, id: string, component: WidgetComponent): Disposable
  widgetTypes?(): WidgetType[]
  registerEntityRenderer?(
    plugin: string,
    kind: "card" | "row",
    scheme: string,
    component: EntityRenderer,
  ): Disposable
  entityRenderer?(kind: "card" | "row", scheme: string): EntityRenderer | undefined
  runCommand(id: string, params?: Record<string, unknown>): Promise<unknown>
  openView?(id: string, opts?: OpenViewOptions): void
  openEntity?(uri: string): void
  share?(uri: string): void
  href?(id: string, opts?: OpenViewOptions): string | null
  toast?(t: Toast): void
  space?(): SpaceInfo | null
  provideContext?(id: string, provider: ContextProvider): Disposable
  contextValue?(id: string): string | undefined
  switchContext?(id: string, value: string): void
  me?(): { id: string; name: string } | null
  manifests?(): Manifest[]
  readonly components?: ShellComponents
}

/** Collects a plugin's registrations so they can all be undone together on unload or reload. */
export class ClientScope {
  private readonly items: Disposable[] = []
  constructor(
    readonly rt: ClientRuntime,
    readonly plugin: string,
    readonly manifest: Manifest,
  ) {}
  track<D extends Disposable>(d: D): D {
    this.items.push(d)
    return d
  }
  dispose() {
    for (const d of this.items.splice(0).reverse()) {
      try {
        d.dispose()
      } catch (e) {
        console.error(`[${this.plugin}] dispose failed`, e)
      }
    }
  }
}

/** What defineClient returns. */
export interface ClientPlugin {
  readonly __rookery: "client"
  readonly activate: (rookery: any) => unknown
}

export function isClientPlugin(v: unknown): v is ClientPlugin {
  return !!v && typeof v === "object" && (v as ClientPlugin).__rookery === "client"
}

export function createClientApi(rt: ClientRuntime, plugin: string, manifest: Manifest) {
  const scope = new ClientScope(rt, plugin, manifest)
  return { rookery: createClientRookery(scope), dispose: () => scope.dispose() }
}
