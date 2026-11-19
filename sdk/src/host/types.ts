// Host-half API types for Rookery plugins: the contract plugins build their host halves against.
// `runtime.ts` is what the extension host runs them with.
import type { CollectionsOf, Refs, Row } from "../data"
import type {
  AccessLevel,
  AppServiceInfo,
  AuditEntry,
  BlobInfo,
  Checkout,
  CheckoutCreate,
  CheckoutRemove,
  ConnectionInfo,
  ConnectionResolved,
  DirListing,
  EnvInfo,
  EnvPresence,
  ExecResult,
  ExtensionContribution,
  FileData,
  FileStat,
  FileWritten,
  JobContext,
  JobDetail,
  JobInfo,
  LeaseGrant,
  Manifest,
  Principal,
  ScheduleInfo,
  ScreenInfo,
  SettingScope,
  SpaceInfo,
  StorageScope,
  TerminalInfo,
  TopicMessage,
  User,
  WorktreeCreate,
} from "../generated"
import type { MethodDecls, NoExtraMethods, ServiceDecl, ServiceImpl } from "../interface"
import type {
  Collections,
  CommandId,
  Disposable,
  SettingKey,
  TopicPayloads,
  Settings as TypedSettings,
} from "../types"
import type { ServerEventName, ServerEvents } from "../wire"

export interface Logger {
  debug(message: string, fields?: Record<string, unknown>): void
  info(message: string, fields?: Record<string, unknown>): void
  warn(message: string, fields?: Record<string, unknown>): void
  error(message: string, fields?: Record<string, unknown>): void
}

export interface Ctx {
  /** The invocation the handler is serving, which the server knows the person by. */
  readonly callId: string
  readonly user: User | null
  readonly principal: Principal
  readonly space: string | null
  readonly signal: AbortSignal
  readonly data: CollectionsOf<Collections> & {
    readonly refs: Refs
    space(id: string): CollectionsOf<Collections>
  }
  readonly access: Access
}

export interface Access {
  check(resource: string, level: AccessLevel): Promise<boolean>
  require(resource: string, level: AccessLevel): Promise<void>
  level(resource: string): Promise<AccessLevel | null>
  filter(resources: string[], level: AccessLevel): Promise<string[]>
  grant(resource: string, subject: string, level: AccessLevel): Promise<void>
  revoke(resource: string, subject: string): Promise<void>
}

export interface HostCommandDef<P = any, R = unknown> {
  params?: { parse(input: unknown): P }
  run(ctx: Ctx, params: P): R | Promise<R>
}

export interface Commands {
  register<P = Record<string, unknown>, R = unknown>(id: CommandId, def: HostCommandDef<P, R>): Disposable
  run<T = unknown>(ctx: Ctx, id: CommandId, params?: Record<string, unknown>): Promise<T>
}

/**
 * Who may call a method of `rookery.rpc.handle` through `plugins.rpc`: `own`, anyone signed in, or
 * `admin`. The server checks it before the handler runs; a handler of level `own` decides the rest.
 */
export type RpcLevel = "own" | "admin"

export interface RpcHandlerDef<P = any, R = unknown> {
  /** Every handler says who may call it. */
  level: RpcLevel
  params?: { parse(input: unknown): P }
  run(ctx: Ctx, params: P): R | Promise<R>
}

export interface Rpc {
  handle<P = Record<string, unknown>, R = unknown>(method: string, def: RpcHandlerDef<P, R>): Disposable
}

export interface Services {
  provide<N extends string, M extends MethodDecls, I extends ServiceImpl<M>>(
    service: ServiceDecl<N, M>,
    methods: I & NoExtraMethods<I, M>,
  ): Disposable
  provide(service: string, methods: Record<string, (ctx: Ctx, params: any) => unknown>): Disposable
  call<T = unknown>(
    plugin: string,
    service: string,
    method: string,
    params?: Record<string, unknown>,
  ): Promise<T>
  stream<T = unknown>(
    plugin: string,
    service: string,
    method: string,
    params?: Record<string, unknown>,
  ): AsyncIterable<T>
}

export interface Extensions {
  list(point: string): Promise<ExtensionContribution[]>
}

export interface Events {
  /** Publishes an event of this plugin, `PLUGIN/topic` to subscribers. */
  publish(topic: string, payload?: unknown): Promise<void>
  /**
   * Subscribes to an event of the server by its full name, such as `rookery/jobs.changed`, with its
   * payload typed, or to this plugin's own by a name without a `/`, or to another plugin's by its full
   * name.
   */
  subscribe<K extends ServerEventName>(
    topic: K,
    handler: (payload: ServerEvents[K], topic: string) => unknown,
  ): Promise<Disposable>
  subscribe(topic: string, handler: (payload: unknown, topic: string) => unknown): Promise<Disposable>
}

export interface Settings {
  get<K extends SettingKey>(key: K, opts?: { space?: string; user?: string }): Promise<TypedSettings[K]>
  set<K extends SettingKey>(
    key: K,
    value: TypedSettings[K],
    opts?: { scope?: SettingScope; space?: string; user?: string },
  ): Promise<void>
  reset(key: SettingKey, opts?: { scope?: SettingScope; space?: string; user?: string }): Promise<void>
}

export interface ChangeEvent {
  collection: string
  op: "insert" | "update" | "delete" | "restore" | "purge"
  id: string
  row: Row | null
  seq: number
}

export interface Changes {
  subscribe(collection: string, handler: (change: ChangeEvent) => unknown): Promise<Disposable>
}

export type ServiceData = CollectionsOf<Collections> & {
  readonly refs: Refs
  readonly changes: Changes
  space(id: string): CollectionsOf<Collections>
}

export interface StorageOptions {
  scope?: StorageScope
  space?: string
  user?: string
  expectedRevision?: number
}

export interface Storage {
  get<T = unknown>(key: string, opts?: Omit<StorageOptions, "expectedRevision">): Promise<T | null>
  set(key: string, value: unknown, opts?: StorageOptions): Promise<void>
  list(
    opts?: Omit<StorageOptions, "expectedRevision"> & { prefix?: string },
  ): Promise<Array<{ key: string; value: unknown; revision?: number }>>
  delete(key: string, opts?: Omit<StorageOptions, "expectedRevision">): Promise<void>
}

export interface SecretOptions {
  scope?: StorageScope
  space?: string
}

export interface Secrets {
  get(name: string, opts?: SecretOptions): Promise<string | null>
  set(name: string, value: string, opts?: SecretOptions): Promise<void>
  list(opts?: SecretOptions): Promise<string[]>
  delete(name: string, opts?: SecretOptions): Promise<void>
  rename(from: string, to: string, opts?: SecretOptions): Promise<void>
}

export interface Connections {
  list(opts?: { type?: string; space?: string }): Promise<ConnectionInfo[]>
  get(id: string): Promise<ConnectionInfo>
  resolve(id: string): Promise<ConnectionResolved>
}

export interface BlobOptions {
  contentType?: string
  filename?: string
}

export interface Blobs {
  put(data: Uint8Array, opts?: BlobOptions): Promise<BlobInfo>
  get(id: string): Promise<BlobInfo>
  read(id: string): Promise<Uint8Array>
  delete(id: string): Promise<void>
}

export interface JobHandle {
  readonly id: string
  readonly attempt: number
  readonly signal: AbortSignal
  /** The invocation the job runs, which its env calls name. Its person is none: a job runs for the server. */
  readonly ctx: Ctx
  readonly context: JobContext
  progress(done: number, total?: number, message?: string): Promise<void>
  log(message: string, level?: "info" | "warn" | "error"): Promise<void>
}

export interface JobDef<P = any, R = unknown> {
  params?: { parse(input: unknown): P }
  run(job: JobHandle, params: P): R | Promise<R>
}

export interface EnqueueOptions {
  delayMs?: number
  dedupeKey?: string
}

export interface Jobs {
  handle<P = Record<string, unknown>, R = unknown>(name: string, def: JobDef<P, R>): Disposable
  enqueue(name: string, params?: Record<string, unknown>, opts?: EnqueueOptions): Promise<JobInfo>
  get(id: string): Promise<JobDetail>
  cancel(id: string): Promise<void>
}

export interface LeaseOptions {
  ttlMs?: number
}

export interface Lease {
  readonly key: string
  readonly token: number
  readonly expiresAt: string
  renew(ttlMs?: number): Promise<LeaseGrant>
  release(): Promise<void>
}

export interface Leases {
  acquire(key: string, opts?: LeaseOptions): Promise<Lease | null>
  with<T>(key: string, fn: (lease: Lease) => Promise<T>, opts?: LeaseOptions): Promise<T>
}

export interface SubscribeOptions {
  consumer: string
  start?: "earliest" | "latest"
}

export interface Topics {
  publish<K extends keyof TopicPayloads & string>(
    topic: K,
    payload: TopicPayloads[K],
  ): Promise<{ seq: number }>
  subscribe<K extends keyof TopicPayloads & string>(
    topic: K,
    opts: SubscribeOptions,
    handler: (msg: TopicMessage) => Promise<void> | void,
  ): Promise<Disposable>
}

export interface Schedules {
  list(): Promise<ScheduleInfo[]>
  setEnabled(id: string, enabled: boolean): Promise<void>
  fire(id: string): Promise<JobInfo>
}

export type NoticeKind = "info" | "warning" | "action"

export interface Notice {
  user: string
  title: string
  body?: string
  kind?: NoticeKind
  resource?: string
}

export interface Notify {
  send(notice: Notice): Promise<{ id: string }>
}

export interface RookeryStream {
  readonly id: number
  write(data: Uint8Array): Promise<void>
  eof(): void
  close(): void
  onData: (data: Uint8Array) => void
  onEnd: (error?: { code: string; message: string; details?: unknown }) => void
}

export interface Streams {
  handle(channel: string, handler: (ctx: Ctx, stream: RookeryStream, params: unknown) => unknown): Disposable
}

export interface HostEnv {
  list(ctx: Ctx): Promise<EnvInfo[]>
  get(ctx: Ctx, env: string): Promise<EnvInfo>
  presence(ctx: Ctx, env: string): Promise<EnvPresence>
  exec(
    ctx: Ctx,
    env: string,
    command: string[],
    opts?: { cwd?: string; env?: Record<string, string>; stdin?: string; timeoutMs?: number },
  ): Promise<ExecResult>
  /** The git checkouts and worktrees under the host's checkouts folder. */
  checkouts(ctx: Ctx, env: string): Promise<Checkout[]>
  /** Clones a repository into the host's checkouts folder, and answers the checkout it made. */
  clone(
    ctx: Ctx,
    env: string,
    repository: string,
    opts?: Omit<CheckoutCreate, "env" | "repository">,
  ): Promise<Checkout>
  /** Makes a worktree of a checkout, on a new branch, in the host's checkouts folder. */
  worktree(
    ctx: Ctx,
    env: string,
    checkout: string,
    branch: string,
    opts?: Omit<WorktreeCreate, "env" | "checkout" | "branch">,
  ): Promise<Checkout>
  /** Removes a checkout, or a worktree through `git worktree remove`. */
  removeCheckout(
    ctx: Ctx,
    env: string,
    path: string,
    opts?: Omit<CheckoutRemove, "env" | "path">,
  ): Promise<void>
  readFile(ctx: Ctx, env: string, path: string): Promise<FileData>
  /** Writes bytes, or text as UTF-8. With `ifMatch`, only while the file is at that SHA-256. */
  writeFile(
    ctx: Ctx,
    env: string,
    path: string,
    content: string | Uint8Array,
    opts?: { ifMatch?: string; overwrite?: boolean },
  ): Promise<FileWritten>
  deleteFile(ctx: Ctx, env: string, path: string, opts?: { recursive?: boolean }): Promise<void>
  listDir(ctx: Ctx, env: string, path: string): Promise<DirListing>
  stat(ctx: Ctx, env: string, path: string): Promise<FileStat>
}

export interface Terminals {
  list(ctx: Ctx, env?: string): Promise<TerminalInfo[]>
  get(ctx: Ctx, terminal: string): Promise<TerminalInfo>
  screen(ctx: Ctx, terminal: string): Promise<ScreenInfo>
  write(ctx: Ctx, terminal: string, data: string): Promise<void>
  close(ctx: Ctx, terminal: string): Promise<void>
}

export interface Audit {
  record(entry: { action: string; resource?: string; details?: Record<string, unknown> }): Promise<void>
  list(query?: { action?: string; limit?: number }): Promise<AuditEntry[]>
}

export interface Spaces {
  list(ctx: Ctx): Promise<SpaceInfo[]>
  get(ctx: Ctx, id: string): Promise<SpaceInfo>
  create(ctx: Ctx, params: { kind: string; name: string }): Promise<SpaceInfo>
}

export interface AppServices {
  list(): Promise<AppServiceInfo[]>
  call<T = unknown>(service: string, method: string, params?: Record<string, unknown>): Promise<T>
}

/** The handle a plugin's host half receives in `definePlugin((rookery) => { ... })`. */
export interface Rookery {
  readonly id: string
  readonly manifest: Manifest
  readonly log: Logger
  readonly commands: Commands
  readonly rpc: Rpc
  readonly services: Services
  readonly extensions: Extensions
  readonly events: Events
  readonly settings: Settings
  readonly data: ServiceData
  readonly spaces: Spaces
  readonly storage: Storage
  readonly secrets: Secrets
  readonly connections: Connections
  readonly blobs: Blobs
  readonly audit: Audit
  readonly jobs: Jobs
  readonly leases: Leases
  readonly topics: Topics
  readonly schedules: Schedules
  readonly notify: Notify
  readonly streams: Streams
  readonly env: HostEnv
  readonly terminals: Terminals
  readonly appServices: AppServices
  onDispose(fn: () => unknown): void
}

/** What definePlugin returns. */
export interface HostPlugin {
  readonly __rookery: "host"
  readonly activate: (rookery: Rookery) => unknown
}

export function isHostPlugin(v: unknown): v is HostPlugin {
  return !!v && typeof v === "object" && (v as HostPlugin).__rookery === "host"
}
